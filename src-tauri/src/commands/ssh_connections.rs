use tauri::State;
use uuid::Uuid;

use crate::{
    cli_tools::{factory::CliToolFactory, CliExecutionContext, CliLocationKind},
    db,
    models::{
        SshConfigHostDto, SshConnectionDto, SshConnectionImportResultDto, SshConnectionInput,
        SshConnectionTestDto,
    },
    ssh::{config, gateway, known_hosts},
    state::AppState,
};

async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_ssh_connections(
    state: State<'_, AppState>,
) -> Result<Vec<SshConnectionDto>, String> {
    run_db(state.db.clone(), |db| db::ssh_connections::list(db, false)).await
}

#[tauri::command]
pub async fn list_deleted_ssh_connections(
    state: State<'_, AppState>,
) -> Result<Vec<SshConnectionDto>, String> {
    run_db(state.db.clone(), |db| db::ssh_connections::list(db, true)).await
}

#[tauri::command]
pub async fn scan_ssh_config_hosts(
    state: State<'_, AppState>,
) -> Result<Vec<SshConfigHostDto>, String> {
    let hosts = config::scan().await.map_err(|e| e.to_string())?;
    run_db(state.db.clone(), move |db| {
        hosts
            .into_iter()
            .map(|host| {
                let imported =
                    db::ssh_connections::find_active_by_alias(db, &host.alias)?.is_some();
                let deleted =
                    db::ssh_connections::find_deleted_by_alias(db, &host.alias)?.is_some();
                Ok(config::as_dto(host, imported, deleted))
            })
            .collect()
    })
    .await
}

#[tauri::command]
pub async fn import_ssh_config_hosts(
    state: State<'_, AppState>,
    aliases: Vec<String>,
) -> Result<Vec<SshConnectionImportResultDto>, String> {
    let hosts = config::scan().await.map_err(|e| e.to_string())?;
    let selected = hosts
        .into_iter()
        .filter(|host| aliases.iter().any(|alias| alias == &host.alias))
        .collect::<Vec<_>>();
    let mut results: Vec<SshConnectionImportResultDto> = run_db(state.db.clone(), move |db| {
        selected
            .into_iter()
            .map(|host| {
                let alias = host.alias.clone();
                if let Some(existing) = db::ssh_connections::find_active_by_alias(db, &alias)? {
                    return Ok(SshConnectionImportResultDto {
                        alias,
                        connection: Some(existing.dto),
                        error: None,
                        restored: false,
                    });
                }
                if let Some(existing) = db::ssh_connections::find_deleted_by_alias(db, &alias)? {
                    let input = SshConnectionInput {
                        display_name: existing.dto.display_name.clone(),
                        host_name: host.host_name.clone(),
                        user: host.user.clone(),
                        port: host.port,
                        identity_file: host.identity_file.clone(),
                        host_key: String::new(),
                        config_alias: Some(alias.clone()),
                    };
                    db::ssh_connections::restore(db, &existing.dto.id)?;
                    let connection =
                        db::ssh_connections::update(db, &existing.dto.id, &input, "config", "")?;
                    return Ok(SshConnectionImportResultDto {
                        alias,
                        connection: Some(connection),
                        error: None,
                        restored: true,
                    });
                }
                let input = SshConnectionInput {
                    display_name: host.alias.clone(),
                    host_name: host.host_name,
                    user: host.user,
                    port: host.port,
                    identity_file: host.identity_file,
                    host_key: String::new(),
                    config_alias: Some(alias.clone()),
                };
                let connection = db::ssh_connections::insert(
                    db,
                    &Uuid::new_v4().to_string(),
                    "ssh_config",
                    &input,
                    "config",
                    "",
                )?;
                Ok(SshConnectionImportResultDto {
                    alias,
                    connection: Some(connection),
                    error: None,
                    restored: false,
                })
            })
            .collect()
    })
    .await?;
    for result in &mut results {
        let Some(connection) = result.connection.as_ref() else {
            continue;
        };
        let record = run_db(state.db.clone(), {
            let connection_id = connection.id.clone();
            move |db| db::ssh_connections::find(db, &connection_id)
        })
        .await?
        .ok_or_else(|| "SSH 连接不存在".to_string())?;
        let test = test_and_register_cli_tunnels(state.inner(), &record).await?;
        if !test.ok {
            result.error = test.error;
        }
    }
    state
        .ssh_monitor
        .reconcile(state.db.clone())
        .await
        .map_err(|error| error.to_string())?;
    Ok(results)
}

#[tauri::command]
pub async fn create_manual_ssh_connection(
    state: State<'_, AppState>,
    input: SshConnectionInput,
) -> Result<SshConnectionDto, String> {
    validate_input(&input, true)?;
    let (key_type, key_base64) =
        known_hosts::parse_host_key(&input.host_key).map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let stored_key_type = key_type.clone();
    let stored_key_base64 = key_base64.clone();
    let dto = run_db(state.db.clone(), move |db| {
        db::ssh_connections::insert(
            db,
            &id,
            "manual",
            &input,
            &stored_key_type,
            &stored_key_base64,
        )
    })
    .await?;
    known_hosts::write(&dto.id, &dto.host_name, dto.port, &key_type, &key_base64)
        .map_err(|e| e.to_string())?;
    let record = run_db(state.db.clone(), {
        let connection_id = dto.id.clone();
        move |db| db::ssh_connections::find(db, &connection_id)
    })
    .await?
    .ok_or_else(|| "SSH 连接不存在".to_string())?;
    let _ = test_and_register_cli_tunnels(state.inner(), &record).await?;
    state
        .ssh_monitor
        .start(state.db.clone(), &dto.id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(dto)
}

#[tauri::command]
pub async fn update_ssh_connection(
    state: State<'_, AppState>,
    connection_id: String,
    input: SshConnectionInput,
) -> Result<SshConnectionDto, String> {
    validate_input(&input, false)?;
    let existing = run_db(state.db.clone(), {
        let id = connection_id.clone();
        move |db| db::ssh_connections::find(db, &id)
    })
    .await?
    .ok_or_else(|| "SSH 连接不存在".to_string())?;
    let (key_type, key_base64) = if input.config_alias.is_some() {
        ("config".to_string(), String::new())
    } else if input.host_key.trim().is_empty() {
        (
            existing.dto.host_key_type.clone(),
            existing.host_key_base64.clone(),
        )
    } else {
        known_hosts::parse_host_key(&input.host_key).map_err(|e| e.to_string())?
    };
    state.ssh_monitor.stop(&connection_id).await;
    let id = connection_id.clone();
    let stored_key_type = key_type.clone();
    let stored_key_base64 = key_base64.clone();
    let dto = run_db(state.db.clone(), move |db| {
        db::ssh_connections::update(db, &id, &input, &stored_key_type, &stored_key_base64)
    })
    .await;
    let dto = match dto {
        Ok(dto) => dto,
        Err(error) => {
            let _ = state
                .ssh_monitor
                .start(state.db.clone(), &connection_id)
                .await;
            return Err(error);
        }
    };
    if dto.source_kind == "manual" {
        known_hosts::write(&dto.id, &dto.host_name, dto.port, &key_type, &key_base64)
            .map_err(|e| e.to_string())?;
    }
    state
        .ssh_monitor
        .start(state.db.clone(), &dto.id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(dto)
}

#[tauri::command]
pub async fn test_ssh_connection(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<SshConnectionTestDto, String> {
    let record = run_db(state.db.clone(), move |db| {
        db::ssh_connections::find(db, &connection_id)
    })
    .await?
    .ok_or_else(|| "SSH 连接不存在".to_string())?;
    let result = gateway::test(&record).await;
    let id = result.connection_id.clone();
    let version = record.dto.updated_at.clone();
    let ok = result.ok;
    let error = result.error.clone();
    run_db(state.db.clone(), move |db| {
        db::ssh_connections::record_test(db, &id, &version, ok, error.as_deref())
    })
    .await?;
    Ok(result)
}

async fn test_and_register_cli_tunnels(
    state: &AppState,
    record: &db::ssh_connections::SshConnectionRecord,
) -> Result<SshConnectionTestDto, String> {
    let result = gateway::test(record).await;
    let id = result.connection_id.clone();
    let version = record.dto.updated_at.clone();
    let ok = result.ok;
    let error = result.error.clone();
    run_db(state.db.clone(), move |db| {
        db::ssh_connections::record_test(db, &id, &version, ok, error.as_deref())
    })
    .await?;
    if result.ok {
        let factory = CliToolFactory::new(state.clone());
        for cli_id in result.cli_versions.keys() {
            let cli = match factory.create(cli_id) {
                Ok(cli) => cli,
                Err(error) => {
                    log::warn!(
                        "创建 SSH CLI 统一接口失败，继续处理其他 CLI: connection_id={} cli_id={} error={error:#}",
                        record.dto.id,
                        cli_id,
                    );
                    continue;
                }
            };
            let context = CliExecutionContext {
                workspace_id: String::new(),
                root_path: String::new(),
                location_kind: CliLocationKind::Ssh,
                ssh_connection_id: Some(record.dto.id.clone()),
            };
            if let Err(error) = cli.register_remote_service(&context).await {
                log::warn!(
                    "注册 SSH 远端 CLI 服务失败，继续处理其他 CLI: connection_id={} cli_id={} error={error:#}",
                    record.dto.id,
                    cli_id,
                );
            }
        }
    }
    Ok(result)
}

#[tauri::command]
pub async fn set_ssh_connection_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
    enabled: bool,
) -> Result<SshConnectionDto, String> {
    let dto = run_db(state.db.clone(), move |db| {
        db::ssh_connections::set_enabled(db, &connection_id, enabled)
    })
    .await?;
    if enabled {
        state
            .ssh_monitor
            .start(state.db.clone(), &dto.id)
            .await
            .map_err(|error| error.to_string())?;
    } else {
        state.ssh_monitor.stop(&dto.id).await;
        close_connection_terminals(&app, state.inner(), &dto.id).await?;
    }
    Ok(dto)
}

#[tauri::command]
pub async fn delete_ssh_connection(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<(), String> {
    let delete_id = connection_id.clone();
    run_db(state.db.clone(), move |db| {
        db::ssh_connections::soft_delete(db, &delete_id)
    })
    .await?;
    state.ssh_monitor.stop(&connection_id).await;
    close_connection_terminals(&app, state.inner(), &connection_id).await?;
    Ok(())
}

async fn close_connection_terminals(
    app: &tauri::AppHandle,
    state: &AppState,
    connection_id: &str,
) -> Result<(), String> {
    let workspace_ids = run_db(state.db.clone(), {
        let connection_id = connection_id.to_string();
        move |db| db::workspaces::workspace_ids_for_ssh_connection(db, &connection_id)
    })
    .await?;
    for workspace_id in workspace_ids {
        state
            .terminals
            .close_workspace(app.clone(), &workspace_id)
            .await
            .map_err(|error| error.to_string())?;
        state
            .notifications
            .clear_for_workspace(app, &workspace_id)
            .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn restore_ssh_connection(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<SshConnectionDto, String> {
    let dto = run_db(state.db.clone(), move |db| {
        db::ssh_connections::restore(db, &connection_id)
    })
    .await?;
    if dto.enabled {
        state
            .ssh_monitor
            .start(state.db.clone(), &dto.id)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(dto)
}

fn validate_input(input: &SshConnectionInput, require_key: bool) -> Result<(), String> {
    if input.display_name.trim().is_empty()
        || input.host_name.trim().is_empty()
        || input.user.trim().is_empty()
    {
        return Err("显示名称、主机地址和用户名不能为空".to_string());
    }
    if input.port == 0 {
        return Err("端口必须在 1-65535 范围内".to_string());
    }
    if require_key && input.host_key.trim().is_empty() {
        return Err("手动添加必须填写完整的 Host Key".to_string());
    }
    Ok(())
}
