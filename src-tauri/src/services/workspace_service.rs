use anyhow::{Context, Result};

use crate::{
    db::{self, Database},
    fs_ops,
    git::repo,
    models::{ExecutionTargetDto, FileTreeEntryDto, FileTreePageDto, TrustLevelDto, WorkspaceDto},
    remote_project_session_refresh_service,
    ssh::{remote_fs, runtime::resolve_workspace_target},
    state::AppState,
    workspace_startup::{
        normalize_workspace_startup_preset as normalize_preset,
        parse_persisted_workspace_startup_preset_json, parse_workspace_startup_preset_raw,
        resolve_workspace_path,
        serialize_workspace_startup_preset as serialize_preset, WorkspaceStartupPreset,
        WorkspaceStartupPresetFormat,
    },
};

/// 集中承载工作区 CRUD、启动预设和文件浏览查询的业务服务。
pub struct WorkspaceService {
    /// 当前应用的数据库和文件树缓存依赖。
    state: AppState,
}

impl WorkspaceService {
    /// 创建工作区业务服务并绑定当前应用状态。
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 根据本机目录打开或登记工作区。
    pub async fn open_workspace(&self, path: String) -> Result<WorkspaceDto> {
        // 在线程池中保存工作区，避免 SQLite 操作阻塞异步运行时。
        let log_path = path.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            db::workspaces::upsert_workspace(db, &path)
        })
        .await
        .context("打开工作区失败");
        if let Err(error) = &result {
            log::error!("打开工作区失败: path={} error={error:#}", log_path);
        }
        result
    }

    /// 查询所有未归档的工作区。
    pub async fn list_workspaces(&self) -> Result<Vec<WorkspaceDto>> {
        // 在线程池中读取工作区列表，避免 SQLite 操作阻塞异步运行时。
        let result = run_db(self.state.db.clone(), db::workspaces::list_workspaces)
            .await
            .context("读取工作区列表失败");
        if let Err(error) = &result {
            log::error!("读取工作区列表失败: error={error:#}");
        }
        result
    }

    /// 查询本机或 SSH 工作区的展示目标，不改变连接的启用或删除状态。
    pub async fn get_execution_target(
        &self,
        workspace_id: Option<String>,
    ) -> Result<ExecutionTargetDto> {
        let log_workspace_id = workspace_id.clone();

        // 在线程池中读取工作区和连接展示信息，避免阻塞异步运行时。
        let result = run_db(self.state.db.clone(), move |db| {
            load_execution_target(db, &workspace_id)
        })
        .await
        .context("读取执行目标失败");
        if let Err(error) = &result {
            log::error!("读取执行目标失败: workspace_id={log_workspace_id:?} error={error:#}");
        }
        result
    }

    /// 查询所有已归档的工作区。
    pub async fn list_archived_workspaces(&self) -> Result<Vec<WorkspaceDto>> {
        // 在线程池中读取归档工作区列表，避免 SQLite 操作阻塞异步运行时。
        let result = run_db(self.state.db.clone(), db::workspaces::list_archived_workspaces)
            .await
            .context("读取已归档工作区列表失败");
        if let Err(error) = &result {
            log::error!("读取已归档工作区列表失败: error={error:#}");
        }
        result
    }

    /// 解析 SSH 项目根目录并创建工作区，再异步刷新其远端会话。
    pub async fn create_ssh_workspace(
        &self,
        app: tauri::AppHandle,
        connection_id: String,
        name: String,
        root_path: String,
    ) -> Result<WorkspaceDto> {
        // 先通过 SSH 连接服务确认目录，再持久化工作区并启动原有会话刷新任务。
        let log_connection_id = connection_id.clone();
        let log_name = name.clone();
        let result = async {
            let directory = crate::services::ssh_connection_service::SshConnectionService::new(
                self.state.clone(),
            )
            .resolve_ssh_directory(connection_id.clone(), root_path, None)
            .await
            .context("解析 SSH 工作区根目录失败")?;
            let workspace = run_db(self.state.db.clone(), move |db| {
                db::workspaces::create_ssh_workspace(db, &connection_id, &name, &directory.path)
            })
            .await
            .context("创建 SSH 工作区失败")?;

            let sync_app = app.clone();
            let sync_db = self.state.db.clone();
            let workspace_id = workspace.id.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = remote_project_session_refresh_service::refresh_ssh_remote_project_sessions(
                    &sync_app,
                    sync_db.into(),
                    &workspace_id,
                )
                .await
                {
                    log::warn!(
                        "新增 SSH 远端项目后刷新会话失败: workspace_id={} error={error:#}",
                        workspace_id
                    );
                }
            });

            Ok(workspace)
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "创建 SSH 工作区失败: connection_id={} name={} error={error:#}",
                log_connection_id,
                log_name
            );
        }
        result
    }

    /// 更新工作区的项目级信任等级。
    pub async fn set_workspace_trust_level(
        &self,
        workspace_id: String,
        trust_level: TrustLevelDto,
    ) -> Result<()> {
        // 在线程池中保存信任等级，避免 SQLite 操作阻塞异步运行时。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            db::workspaces::set_workspace_trust_level(db, &workspace_id, &trust_level)
        })
        .await
        .context("设置工作区信任等级失败");
        if let Err(error) = &result {
            log::error!(
                "设置工作区信任等级失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 按现有 Mapper 规则物理删除工作区及其级联数据。
    pub async fn delete_workspace(&self, workspace_id: String) -> Result<()> {
        // 在线程池中执行原有工作区物理删除操作。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            db::workspaces::delete_workspace(db, &workspace_id)
        })
        .await
        .context("删除工作区失败");
        if let Err(error) = &result {
            log::error!(
                "删除工作区失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 将工作区标记为已归档。
    pub async fn archive_workspace(&self, workspace_id: String) -> Result<()> {
        // 在线程池中保存工作区归档状态。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            db::workspaces::archive_workspace(db, &workspace_id)
        })
        .await
        .context("归档工作区失败");
        if let Err(error) = &result {
            log::error!(
                "归档工作区失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 恢复已归档的工作区并返回更新后的工作区信息。
    pub async fn restore_workspace(&self, workspace_id: String) -> Result<WorkspaceDto> {
        // 在线程池中恢复工作区，保留 Mapper 原有状态转换规则。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            db::workspaces::restore_workspace(db, &workspace_id)
        })
        .await
        .context("恢复工作区失败");
        if let Err(error) = &result {
            log::error!(
                "恢复工作区失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 读取工作区已保存的启动预设。
    pub async fn get_workspace_startup_preset(
        &self,
        workspace_id: String,
    ) -> Result<Option<WorkspaceStartupPreset>> {
        // 在线程池中先确认工作区存在，再解析已保存的 JSON 预设。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            load_workspace(db, &workspace_id)?;
            db::workspaces::get_workspace_startup_preset_json(db, &workspace_id)?
                .as_deref()
                .map(parse_persisted_workspace_startup_preset_json)
                .transpose()
        })
        .await
        .context("读取工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "读取工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 按所属工作区的本机或 SSH 规则规范化启动预设。
    pub async fn normalize_workspace_startup_preset(
        &self,
        workspace_id: String,
        preset: WorkspaceStartupPreset,
    ) -> Result<WorkspaceStartupPreset> {
        // 在线程池中读取工作区并执行对应位置类型的预设规范化。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            let workspace = load_workspace(db, &workspace_id)?;
            normalize_preset_for_workspace(&workspace, preset)
        })
        .await
        .context("规范化工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "规范化工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 按所属工作区规则规范化预设并转换为指定 JSON 或 TOML 格式。
    pub async fn serialize_workspace_startup_preset(
        &self,
        workspace_id: String,
        preset: WorkspaceStartupPreset,
        format: WorkspaceStartupPresetFormat,
    ) -> Result<String> {
        // 在线程池中完成工作区校验、预设规范化和格式序列化。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            let workspace = load_workspace(db, &workspace_id)?;
            let normalized = normalize_preset_for_workspace(&workspace, preset)?;
            serialize_preset(&normalized, format)
        })
        .await
        .context("序列化工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "序列化工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 将原始 JSON 或 TOML 文本解析后按所属工作区规则规范化。
    pub async fn normalize_workspace_startup_preset_raw(
        &self,
        workspace_id: String,
        format: WorkspaceStartupPresetFormat,
        raw_text: String,
    ) -> Result<WorkspaceStartupPreset> {
        // 在线程池中按指定格式解析文本，再执行工作区预设校验。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            let workspace = load_workspace(db, &workspace_id)?;
            let parsed = parse_workspace_startup_preset_raw(format, &raw_text)?;
            normalize_preset_for_workspace(&workspace, parsed)
        })
        .await
        .context("解析并规范化工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "解析并规范化工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 规范化并保存工作区启动预设，返回持久化的预设内容。
    pub async fn set_workspace_startup_preset(
        &self,
        workspace_id: String,
        preset: WorkspaceStartupPreset,
    ) -> Result<WorkspaceStartupPreset> {
        // 在线程池中先规范化并序列化，再写入 JSON 字段。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            let workspace = load_workspace(db, &workspace_id)?;
            let normalized = normalize_preset_for_workspace(&workspace, preset)?;
            let raw_json = serde_json::to_string(&normalized)
                .context("序列化工作区启动预设 JSON 失败")?;
            db::workspaces::set_workspace_startup_preset_json(db, &workspace_id, Some(&raw_json))?;
            Ok(normalized)
        })
        .await
        .context("保存工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "保存工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 将原始 JSON 或 TOML 文本解析、规范化后保存为工作区启动预设。
    pub async fn set_workspace_startup_preset_raw(
        &self,
        workspace_id: String,
        format: WorkspaceStartupPresetFormat,
        raw_text: String,
    ) -> Result<WorkspaceStartupPreset> {
        // 在线程池中完成文本解析、规范化、JSON 序列化和持久化。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            let workspace = load_workspace(db, &workspace_id)?;
            let parsed = parse_workspace_startup_preset_raw(format, &raw_text)?;
            let normalized = normalize_preset_for_workspace(&workspace, parsed)?;
            let raw_json = serde_json::to_string(&normalized)
                .context("序列化工作区启动预设 JSON 失败")?;
            db::workspaces::set_workspace_startup_preset_json(db, &workspace_id, Some(&raw_json))?;
            Ok(normalized)
        })
        .await
        .context("解析并保存工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "解析并保存工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 清除工作区已保存的启动预设。
    pub async fn clear_workspace_startup_preset(&self, workspace_id: String) -> Result<()> {
        // 在线程池中清空原有 JSON 配置字段。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            db::workspaces::set_workspace_startup_preset_json(db, &workspace_id, None)
        })
        .await
        .context("清除工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "清除工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 导出已保存的工作区启动预设为 JSON 或 TOML 文本。
    pub async fn export_workspace_startup_preset(
        &self,
        workspace_id: String,
        format: WorkspaceStartupPresetFormat,
    ) -> Result<String> {
        // 在线程池中确认配置存在后解析 JSON 并转换到目标格式。
        let log_workspace_id = workspace_id.clone();
        let result = run_db(self.state.db.clone(), move |db| {
            load_workspace(db, &workspace_id)?;
            let raw_json = db::workspaces::get_workspace_startup_preset_json(db, &workspace_id)?
                .ok_or_else(|| anyhow::anyhow!("工作区尚未配置启动预设"))?;
            let preset = parse_persisted_workspace_startup_preset_json(&raw_json)?;
            serialize_preset(&preset, format)
        })
        .await
        .context("导出工作区启动预设失败");
        if let Err(error) = &result {
            log::error!(
                "导出工作区启动预设失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 查询本机或 SSH 工作区中的子目录列表。
    pub async fn list_workspace_dirs(
        &self,
        workspace_id: String,
        dir_path: Option<String>,
    ) -> Result<Vec<FileTreeEntryDto>> {
        // 先解析工作区目标，再按目标位置访问目录并仅返回子目录项。
        let log_workspace_id = workspace_id.clone();
        let result = async {
            let target = run_db(self.state.db.clone(), {
                let workspace_id = workspace_id.clone();
                move |db| resolve_workspace_target(db, &workspace_id)
            })
            .await
            .context("读取工作区目录目标失败")?;
            if target.is_remote() {
                let connection = target
                    .remote_connection()
                    .context("读取 SSH 工作区连接失败")?;
                let entries = remote_fs::list_dir(
                    connection,
                    &target.workspace.root_path,
                    dir_path.as_deref().unwrap_or(""),
                )
                .await
                .context("读取远端工作区目录失败")?;
                return Ok(entries.into_iter().filter(|entry| entry.is_dir).collect());
            }
            let root_path = target.workspace.root_path;
            run_db(self.state.db.clone(), move |_db| {
                let mut entries = fs_ops::list_dir(&root_path, dir_path.as_deref().unwrap_or(""))?;
                entries.retain(|entry| entry.is_dir);
                Ok(entries)
            })
            .await
            .context("读取本机工作区目录失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "读取工作区目录失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 分页读取本机或 SSH 工作区的文件树。
    pub async fn get_workspace_file_tree_page(
        &self,
        workspace_id: String,
        offset: Option<usize>,
        limit: Option<usize>,
        refresh: Option<bool>,
    ) -> Result<FileTreePageDto> {
        // 先解析目标位置，再调用远端文件系统或本机缓存文件树实现。
        let log_workspace_id = workspace_id.clone();
        let cache = self.state.file_tree_cache.clone();
        let result = async {
            let target = run_db(self.state.db.clone(), {
                let workspace_id = workspace_id.clone();
                move |db| resolve_workspace_target(db, &workspace_id)
            })
            .await
            .context("读取工作区文件树目标失败")?;
            if target.is_remote() {
                let connection = target
                    .remote_connection()
                    .context("读取 SSH 工作区连接失败")?;
                return remote_fs::file_tree_page(
                    connection,
                    &target.workspace.root_path,
                    offset.unwrap_or(0),
                    limit.unwrap_or(2000),
                )
                .await
                .context("读取远端工作区文件树失败");
            }
            let root_path = target.workspace.root_path;
            run_db(self.state.db.clone(), move |_db| {
                if refresh.unwrap_or(false) {
                    cache.invalidate_workspace(&root_path);
                }
                repo::get_workspace_file_tree_page(
                    &root_path,
                    offset.unwrap_or(0),
                    limit.unwrap_or(2000),
                    &cache,
                )
            })
            .await
            .context("读取本机工作区文件树失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "读取工作区文件树失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }

    /// 按查询词分页搜索本机或 SSH 工作区文件。
    pub async fn search_workspace_files(
        &self,
        workspace_id: String,
        query: String,
        offset: Option<usize>,
        limit: Option<usize>,
        refresh: Option<bool>,
    ) -> Result<FileTreePageDto> {
        // 先解析目标位置，再按本机或远端实现执行搜索。
        let log_workspace_id = workspace_id.clone();
        let cache = self.state.file_tree_cache.clone();
        let result = async {
            let target = run_db(self.state.db.clone(), {
                let workspace_id = workspace_id.clone();
                move |db| resolve_workspace_target(db, &workspace_id)
            })
            .await
            .context("读取工作区搜索目标失败")?;
            if target.is_remote() {
                let connection = target
                    .remote_connection()
                    .context("读取 SSH 工作区连接失败")?;
                return remote_fs::search_files(
                    connection,
                    &target.workspace.root_path,
                    &query,
                    offset.unwrap_or(0),
                    limit.unwrap_or(80),
                )
                .await
                .context("搜索远端工作区文件失败");
            }
            let root_path = target.workspace.root_path;
            run_db(self.state.db.clone(), move |_db| {
                if refresh.unwrap_or(false) {
                    cache.invalidate_workspace(&root_path);
                }
                repo::search_workspace_files(
                    &root_path,
                    &query,
                    offset.unwrap_or(0),
                    limit.unwrap_or(80),
                    &cache,
                )
            })
            .await
            .context("搜索本机工作区文件失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "搜索工作区文件失败: workspace_id={} error={error:#}",
                log_workspace_id
            );
        }
        result
    }
}

/// 在线程池运行同步 SQLite 操作，避免阻塞异步业务服务。
async fn run_db<T, F>(db: Database, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&Database) -> Result<T> + Send + 'static,
{
    // 保留 spawn_blocking 的原始 JoinError 链，供业务方法写入完整日志。
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .context("数据库任务执行失败")?
}

/// 根据工作区标识加载本机或 SSH 执行目标的展示 DTO。
fn load_execution_target(
    db: &Database,
    workspace_id: &Option<String>,
) -> Result<ExecutionTargetDto> {
    // 未指定工作区时返回本机默认展示目标。
    let Some(workspace_id) = workspace_id else {
        return Ok(ExecutionTargetDto {
            target_key: "local".to_string(),
            kind: "local".to_string(),
            display_name: "本机".to_string(),
            connection_id: None,
            host_name: None,
            user: None,
            port: None,
            project_path: None,
            connection_status: Some("ok".to_string()),
        });
    };

    // 先查询工作区；本机工作区直接展示根目录路径。
    let workspace = db::workspaces::find_workspace_by_id(db, workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace not found: {workspace_id}"))?;
    if workspace.location_kind != "ssh" {
        return Ok(ExecutionTargetDto {
            target_key: "local".to_string(),
            kind: "local".to_string(),
            display_name: "本机".to_string(),
            connection_id: None,
            host_name: None,
            user: None,
            port: None,
            project_path: Some(workspace.root_path),
            connection_status: Some("ok".to_string()),
        });
    }

    // SSH 工作区只读取连接展示资料，不拒绝已禁用或已删除连接。
    let connection_id = workspace
        .ssh_connection_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("远端项目未绑定 SSH 连接"))?
        .to_string();
    let connection = db::ssh_connections::find(db, &connection_id)?
        .ok_or_else(|| anyhow::anyhow!("SSH connection not found: {connection_id}"))?;
    Ok(ExecutionTargetDto {
        target_key: format!("ssh:{connection_id}"),
        kind: "ssh".to_string(),
        display_name: connection.dto.display_name,
        connection_id: Some(connection_id),
        host_name: Some(connection.dto.host_name),
        user: Some(connection.dto.user),
        port: Some(connection.dto.port),
        project_path: Some(workspace.root_path),
        connection_status: Some(connection.dto.connection_status),
    })
}

/// 按工作区 ID 读取 DTO，并在记录不存在时返回业务错误。
fn load_workspace(db: &Database, workspace_id: &str) -> Result<WorkspaceDto> {
    db::workspaces::find_workspace_by_id(db, workspace_id)?
        .ok_or_else(|| anyhow::anyhow!("工作区不存在: {workspace_id}"))
}

/// 按本机或远端工作区规则校验并规范化启动预设。
fn normalize_preset_for_workspace(
    workspace: &WorkspaceDto,
    mut preset: WorkspaceStartupPreset,
) -> Result<WorkspaceStartupPreset> {
    // SSH 工作区仅校验远端路径约束，不在本机规范化远端根目录。
    if workspace.location_kind == "ssh" {
        if let Some(terminal) = preset.terminal.as_mut() {
            for group in &mut terminal.groups {
                if let Some(worktree) = group.worktree.as_mut() {
                    if let Some(base_dir) = worktree.base_dir.as_mut() {
                        *base_dir = base_dir.trim().trim_end_matches('/').to_string();
                        anyhow::ensure!(!base_dir.is_empty(), "远端工作树目录不能为空");
                        anyhow::ensure!(
                            !base_dir.contains('\0')
                                && !base_dir.contains('\n')
                                && !base_dir.contains('\r')
                                && !base_dir.starts_with("ssh://"),
                            "远端工作树目录无效"
                        );
                        if !base_dir.starts_with('/') {
                            crate::ssh::runtime::validate_remote_relative_path(base_dir, false)?;
                        }
                    }
                }
                for session in &mut group.sessions {
                    if matches!(
                        session.cwd_base,
                        Some(crate::workspace_startup::WorkspacePathBase::Absolute)
                    ) {
                        anyhow::bail!("远端启动终端不能使用未登记的绝对目录");
                    }
                }
            }
        }
        let encoded = serde_json::to_string(&preset)?;
        anyhow::ensure!(
            !encoded.contains('\0') && !encoded.contains('\n') && !encoded.contains('\r'),
            "远端启动配置包含非法字符"
        );
        return Ok(preset);
    }
    let workspace_root = resolve_workspace_path(&workspace.root_path)?;
    normalize_preset(preset, &workspace_root)
}

#[cfg(test)]
#[path = "../../tests/unit/workspace_service_tests.rs"]
mod tests;
