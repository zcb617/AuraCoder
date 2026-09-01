mod auracoder_thread_mcp_service;
mod cli_service_health;
pub(crate) mod cli_tools;
mod commands;
mod computer_control_sdk;
mod computer_control_service;
mod config;
mod db;
mod engines;
mod extensions;
mod fs_ops;
mod git;
#[cfg(any(target_os = "linux", test))]
mod linux_appimage;
mod linux_webkit;
mod local_cli_service_lifecycle;
mod locale;
mod mcp_gateway;
pub(crate) mod message_notify_helper;
mod models;
mod path_utils;
mod power;
mod process_utils;
mod remote;
mod remote_project_claude_runtime_service;
mod remote_project_codex_runtime_service;
mod remote_project_opencode_runtime_service;
mod remote_project_session_refresh_service;
mod runtime_env;
mod scheduled_tasks;
mod ssh;
mod state;
mod terminal;
mod terminal_notifications;
mod update;
mod workspace_startup;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use anyhow::Context;
use rusqlite::OptionalExtension;

use cli_tools::{factory::CliToolFactory, CliTool};
use config::app_config::AppConfig;
use db::Database;
use engines::{CodexRuntimeEvent, EngineManager};
use extensions::refresh::{spawn_catalog_refresh_scheduler, ExtensionCatalogRefreshManager};
use git::repo::FileTreeCache;
use git::watcher::GitWatcherManager;
#[cfg(target_os = "macos")]
use locale::native_strings;
use locale::{
    resolve_app_locale, scheduled_exit_confirmation_strings, tray_menu_strings,
    ScheduledExitConfirmationStrings,
};
use models::{EngineRuntimeUpdatedDto, ThreadDto, ThreadStatusDto};
use power::KeepAwakeManager;
use remote::RemoteTunnelManager;
use scheduled_tasks::ScheduledTaskManager;
use state::{AppState, TurnManager};
#[cfg(target_os = "macos")]
use tauri::menu::{AboutMetadata, PredefinedMenuItem, SubmenuBuilder};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Listener, Manager, RunEvent, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use terminal::TerminalManager;

#[cfg(all(test, target_os = "windows"))]
#[link(name = "resource", kind = "static")]
unsafe extern "C" {}

pub fn maybe_handle_cli_subcommand() -> anyhow::Result<bool> {
    terminal_notifications::maybe_handle_cli_subcommand()
}

pub fn initialize_from_cli() -> anyhow::Result<()> {
    runtime_env::initialize_from_cli()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::init();
    linux_webkit::apply_webkit_display_workarounds();
    let tauri_context = tauri::generate_context!();

    let db = match Database::init() {
        Ok(db) => db,
        Err(error) => {
            if let Some(version_error) = error.downcast_ref::<db::UnsupportedDatabaseVersion>() {
                show_database_version_error(
                    tauri_context,
                    version_error.current_version,
                    version_error.supported_version,
                );
                return;
            }
            panic!("failed to initialize database: {error:#}");
        }
    };
    match db::threads::reconcile_runtime_state(&db) {
        Ok(report) => {
            if report.messages_marked_interrupted > 0 || report.thread_status_updates > 0 {
                log::info!(
                    "runtime recovery applied: interrupted_messages={}, thread_status_updates={}",
                    report.messages_marked_interrupted,
                    report.thread_status_updates
                );
            }
        }
        Err(error) => {
            log::warn!("runtime recovery failed, continuing startup: {error}");
        }
    }
    let mut app_config = AppConfig::load_or_create().expect("failed to load config");
    if app_config.remote_access.enabled && app_config.remote_access.ensure_identity() {
        app_config
            .save()
            .expect("failed to persist remote access identity");
    }
    let app_locale = resolve_app_locale(app_config.general.locale.as_deref());
    let keep_awake = Arc::new(KeepAwakeManager::new());
    if let Err(error) = keep_awake.reclaim_stale_helpers() {
        log::warn!("failed to reclaim stale keep awake helper: {error}");
    }
    if app_config.power.keep_awake_enabled {
        if let Err(error) =
            tauri::async_runtime::block_on(keep_awake.enable_with_config(&app_config.power))
        {
            log::warn!("failed to reapply keep awake on startup: {error}");
        }
    }

    let computer_control_sdk = Arc::new(computer_control_sdk::CuaDriverSdk::new());
    let computer_control_service = Arc::new(computer_control_service::ComputerControlService::new(
        computer_control_sdk.clone(),
    ));
    let auracoder_thread_mcp_service =
        Arc::new(auracoder_thread_mcp_service::AuraCoderThreadMcpService::new(db.clone()));
    let mcp_gateway = Arc::new(mcp_gateway::AuraCoderMcpGateway::new());

    let app_state = AppState {
        db,
        config: Arc::new(app_config),
        config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
        engines: Arc::new(EngineManager::new()),
        git_watchers: Arc::new(GitWatcherManager::default()),
        terminals: Arc::new(TerminalManager::default()),
        notifications: Arc::new(terminal_notifications::TerminalNotificationManager::default()),
        keep_awake,
        turns: Arc::new(TurnManager::default()),
        file_tree_cache: Arc::new(FileTreeCache::new()),
        extension_catalog_refreshes: Arc::new(ExtensionCatalogRefreshManager::default()),
        scheduled_tasks: Arc::new(ScheduledTaskManager::new()),
        computer_control_service,
        mcp_gateway,
        auracoder_thread_mcp_service,
        remote_access: Arc::new(RemoteTunnelManager::default()),
        ssh_monitor: Arc::new(ssh::monitor::SshConnectionMonitor::default()),
    };

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(update::UpdateManager::default())
        .manage(app_state)
        .manage(computer_control_sdk)
        .menu(move |handle| build_app_menu(handle, app_locale))
        .setup(|app| {
            // 旧实现 `.ok()` 会吞掉 resource_dir 获取异常，后续 Claude 生命周期只能
            // 使用安装后不存在的编译期路径：
            // let resource_dir = app.path().resource_dir().ok();
            let resource_dir = match app.path().resource_dir() {
                Ok(resource_dir) => Some(resource_dir),
                Err(error) => {
                    log::error!("AuraCoder 启动时获取 resource_dir 失败: {error:#}");
                    let continue_startup = app
                        .dialog()
                        .message(format!(
                            "AuraCoder 无法获取安装包资源路径（resource_dir）。\n\n这会影响 Claude Code 的正常使用。\n\n错误详情：{error}\n\n是否仍要继续启动 AuraCoder？"
                        ))
                        .title("AuraCoder 启动错误")
                        .kind(MessageDialogKind::Error)
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "继续启动".to_string(),
                            "结束 AuraCoder".to_string(),
                        ))
                        .blocking_show();
                    if !continue_startup {
                        app.handle().exit(1);
                        return Ok(());
                    }
                    None
                }
            };
            let main_window = create_main_window(app.handle())?;
            #[cfg(not(target_os = "linux"))]
            let _ = &main_window;

            build_system_tray(app, app_locale, main_window.clone())?;

            #[cfg(target_os = "linux")]
            {
                if let Ok(icon) = Image::from_bytes(include_bytes!("../icons/icon.png")) {
                    if let Err(error) = main_window.set_icon(icon) {
                        log::warn!("failed to apply linux window icon: {error}");
                    }
                }
            }

            #[cfg(target_os = "linux")]
            tauri::async_runtime::spawn_blocking(|| {
                match linux_appimage::ensure_appimage_desktop_integration() {
                    Ok(status) => {
                        if !matches!(
                            status,
                            linux_appimage::AppImageIntegrationStatus::SkippedNotAppImage
                        ) {
                            log::info!("linux AppImage desktop integration status: {status:?}");
                        }
                    }
                    Err(error) => {
                        log::warn!("failed to ensure linux AppImage desktop integration: {error}");
                    }
                }
            });

            let handle = app.handle().clone();
            let state = app.state::<AppState>().inner().clone();
            let computer_control_sdk = app
                .state::<Arc<computer_control_sdk::CuaDriverSdk>>()
                .inner()
                .clone();
            computer_control_sdk.set_resource_dir(resource_dir.clone());
            if state.config.computer_control.enabled {
                if let Err(error) = computer_control_sdk.initialize() {
                    log::error!("failed to initialize CUA SDK during AuraCoder startup: {error}");
                }
                if let Err(error) = computer_control_sdk.restore_wayland_helper_if_installed() {
                    log::warn!(
                        "failed to restore installed CUA Wayland helper during AuraCoder startup: {error}"
                    );
                }
            }
            if let Err(error) = tauri::async_runtime::block_on(state.mcp_gateway.start()) {
                log::error!("AuraCoder MCP Gateway 启动失败: {error}");
            }
            tauri::async_runtime::block_on(
                local_cli_service_lifecycle::LocalCliServiceLifecycle::bind_mcp_gateway(
                    state.mcp_gateway.clone(),
                    Arc::new(cli_tools::factory::CliToolFactory::new(state.clone())),
                ),
            );
            tauri::async_runtime::block_on(ssh::cli_service_lifecycle::bind_mcp_gateway(
                state.mcp_gateway.clone(),
                Arc::new(cli_tools::factory::CliToolFactory::new(state.clone())),
            ));
            if let Err(error) =
                tauri::async_runtime::block_on(state.notifications.start(handle.clone()))
            {
                log::warn!("failed to start terminal notification ingress: {error}");
            }
            state
                .computer_control_service
                .bind_app_handle(handle.clone());
            state
                .engines
                .set_computer_control_service(state.computer_control_service.clone());
            state.engines.set_auracoder_thread_mcp_service(
                state.auracoder_thread_mcp_service.clone(),
            );
            // 旧调用会移动 resource_dir，导致本地 CLI 生命周期拿不到同一资源根目录：
            // state.engines.set_resource_dir(resource_dir);
            state.engines.set_resource_dir(resource_dir.clone());
            if let Err(error) = tauri::async_runtime::block_on(
                local_cli_service_lifecycle::LocalCliServiceLifecycle::init(resource_dir),
            ) {
                log::warn!("AuraCoder 启动时初始化本地 CLI 服务失败: {error:#}");
            }
            tauri::async_runtime::spawn(run_codex_runtime_bridge(handle.clone(), state.clone()));
            spawn_catalog_refresh_scheduler(handle.clone(), state.clone());
            cli_service_health::spawn_cli_service_health_scheduler(
                handle.clone(),
                state.db.clone(),
            );
            state
                .scheduled_tasks
                .clone()
                .start(handle.clone(), state.clone());
            let remote_config = state.config.remote_access.clone();
            let remote_manager = state.remote_access.clone();
            let remote_state = state.clone();
            let remote_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                remote_manager
                    .configure(remote_handle, remote_state, remote_config)
                    .await;
            });
            let ssh_monitor = state.ssh_monitor.clone();
            let ssh_db = state.db.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = ssh_monitor.reconcile(ssh_db).await {
                log::warn!("failed to start SSH connection monitors: {error}");
            }
        });
        let ssh_startup_handle = handle.clone();
        let ssh_startup_db = state.db.clone();
        handle.once("ssh-remote-project-session-sync-ready", move |_| {
            let app_handle = ssh_startup_handle.clone();
            let db = ssh_startup_db.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = message_notify_helper::notify_app_startup_progress(
                    &app_handle,
                    "connecting-ssh",
                    "正在建立 SSH 连接……",
                ) {
                    log::warn!("发送启动进度失败: {error:#}");
                }

                match ssh::cli_tunnel_registry::init_all_ssh_remote_server(
                    &app_handle,
                    db.clone().into(),
                )
                .await
                {
                    Ok(initialization) => {
                        for result in initialization {
                            if let Some(error) = result.error {
                                log::warn!(
                                    "SSH remote server Map recovery completed with errors: connection_id={} error={error}",
                                    result.connection_id,
                                );
                            }
                        }
                        if let Err(error) = message_notify_helper::notify_app_startup_progress(
                            &app_handle,
                            "starting-cli-services",
                            "正在启动远端 CLI 服务……",
                        ) {
                            log::warn!("发送启动进度失败: {error:#}");
                        }
                        remote_project_session_refresh_service::refresh_all_ssh_remote_project_sessions(
                            &app_handle,
                            db.into(),
                        )
                        .await;
                    }
                    Err(error) => {
                        log::warn!("failed to initialize SSH remote server tunnel Map: {error:#}");
                    }
                }

                if let Err(error) = message_notify_helper::notify_app_startup_progress(
                    &app_handle,
                    "completed",
                    "启动完成",
                ) {
                    log::warn!("发送启动完成通知失败: {error:#}");
                }
            });
        });
        app.on_menu_event(move |_app, event| {
                let id = event.id().as_ref();
                match id {
                    "toggle-sidebar" | "toggle-git-panel" | "toggle-focus-mode"
                    | "toggle-fullscreen" | "toggle-search" | "toggle-terminal"
                    | "close-window" | "edit-undo" | "edit-redo" | "edit-cut" | "edit-copy"
                    | "edit-paste" | "edit-select-all" => {
                        let _ = handle.emit("menu-action", id);
                    }
                    _ => {}
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::get_app_locale,
            commands::app::set_app_locale,
            commands::app::get_app_theme,
            commands::app::set_app_theme,
            commands::app::get_display_scale,
            commands::app::set_display_scale,
            commands::computer_control_settings::respond_computer_control_approval,
            commands::computer_control_settings::get_computer_control_settings_status,
            commands::computer_control_settings::set_computer_control_enabled,
            commands::computer_control_settings::install_computer_control_wayland_helper,
            commands::computer_control_settings::revoke_computer_control_authorization,
            commands::power::get_keep_awake_state,
            commands::power::set_keep_awake_enabled,
            commands::power::get_power_settings,
            commands::power::set_power_settings,
            commands::power::get_helper_status,
            commands::power::register_keep_awake_helper,
            commands::remote::get_remote_access_status,
            commands::remote::set_remote_access_enabled,
            commands::remote::regenerate_remote_access_identity,
            commands::remote::refresh_remote_pairing_token,
            commands::remote::revoke_remote_device,
            commands::mcp_gateway::get_mcp_service_status,
            commands::ssh_connections::list_ssh_connections,
            commands::ssh_connections::list_deleted_ssh_connections,
            commands::ssh_connections::scan_ssh_config_hosts,
            commands::ssh_connections::import_ssh_config_hosts,
            commands::ssh_connections::create_manual_ssh_connection,
            commands::ssh_connections::update_ssh_connection,
            commands::ssh_connections::test_ssh_connection,
            commands::ssh_connections::set_ssh_connection_enabled,
            commands::ssh_connections::delete_ssh_connection,
            commands::ssh_connections::restore_ssh_connection,
            commands::browser::browser_show,
            commands::browser::browser_set_bounds,
            commands::browser::browser_hide,
            commands::browser::browser_transfer_scope,
            commands::browser::browser_navigate,
            commands::browser::browser_reload,
            commands::browser::browser_go_back,
            commands::browser::browser_go_forward,
            commands::browser::browser_set_annotation_enabled,
            commands::browser::browser_clear_pending_annotation,
            commands::browser::browser_clear_all_annotations,
            commands::browser::browser_capture_annotation,
            commands::chat::save_pasted_image_attachment,
            commands::chat::read_attachment_preview,
            commands::chat::send_message,
            commands::chat::start_codex_review,
            commands::chat::steer_message,
            commands::chat::cancel_turn,
            commands::chat::restart_remote_cli_service,
            commands::scheduled_tasks::list_scheduled_tasks,
            commands::scheduled_tasks::create_scheduled_task,
            commands::scheduled_tasks::update_scheduled_task,
            commands::scheduled_tasks::set_scheduled_task_enabled,
            commands::scheduled_tasks::acknowledge_scheduled_task,
            commands::scheduled_tasks::delete_scheduled_task,
            commands::chat::respond_to_approval,
            commands::chat::get_thread_messages,
            commands::chat::get_thread_messages_window,
            commands::chat::get_message_blocks,
            commands::chat::get_action_output,
            commands::chat::search_messages,
            commands::workspace::open_workspace,
            commands::workspace::list_workspaces,
            commands::workspace::refresh_local_project_sessions,
            commands::workspace::list_archived_workspaces,
            commands::workspace::get_ssh_connection_home,
            commands::workspace::list_ssh_directories,
            commands::workspace::resolve_ssh_directory,
            commands::workspace::create_ssh_workspace,
            commands::workspace::set_workspace_trust_level,
            commands::git::get_workspace_git_context,
            commands::workspace::archive_workspace,
            commands::workspace::restore_workspace,
            commands::workspace::delete_workspace,
            commands::workspace::get_workspace_startup_preset,
            commands::workspace::normalize_workspace_startup_preset,
            commands::workspace::serialize_workspace_startup_preset,
            commands::workspace::normalize_workspace_startup_preset_raw,
            commands::workspace::set_workspace_startup_preset,
            commands::workspace::set_workspace_startup_preset_raw,
            commands::workspace::clear_workspace_startup_preset,
            commands::workspace::export_workspace_startup_preset,
            commands::workspace::list_workspace_dirs,
            commands::workspace::get_workspace_file_tree_page,
            commands::workspace::search_workspace_files,
            commands::git::get_git_status,
            commands::git::get_file_diff,
            commands::git::get_git_file_compare,
            commands::git::stage_files,
            commands::git::unstage_files,
            commands::git::discard_files,
            commands::git::commit,
            commands::git::soft_reset_last_commit,
            commands::git::fetch_git,
            commands::git::pull_git,
            commands::git::push_git,
            commands::git::list_git_branches,
            commands::git::checkout_git_branch,
            commands::git::create_git_branch,
            commands::git::rename_git_branch,
            commands::git::delete_git_branch,
            commands::git::list_git_commits,
            commands::git::get_commit_diff,
            commands::git::list_git_stashes,
            commands::git::push_git_stash,
            commands::git::apply_git_stash,
            commands::git::pop_git_stash,
            commands::git::get_file_tree,
            commands::git::get_file_tree_page,
            commands::git::add_git_worktree,
            commands::git::list_git_worktrees,
            commands::git::remove_git_worktree,
            commands::git::prune_git_worktrees,
            commands::git::init_git_repo,
            commands::git::list_git_remotes,
            commands::git::add_git_remote,
            commands::git::remove_git_remote,
            commands::git::rename_git_remote,
            commands::app::get_terminal_accelerated_rendering,
            commands::app::set_terminal_accelerated_rendering,
            commands::app::get_terminal_font_size,
            commands::app::set_terminal_font_size,
            commands::app::get_default_autonomy_preset,
            commands::app::set_default_autonomy_preset,
            commands::app::get_agent_notification_settings,
            commands::app::set_chat_notifications_enabled,
            commands::app::set_terminal_notifications_enabled,
            commands::app::install_terminal_notification_integration_command,
            commands::app::set_notification_sound,
            commands::app::preview_notification_sound,
            commands::app::show_agent_notification,
            commands::update::get_update_state,
            commands::update::is_update_downloaded,
            commands::update::check_for_update,
            commands::update::download_update,
            commands::update::install_downloaded_update,
            commands::update::prepare_local_update_for_development,
            commands::files::list_dir,
            commands::files::read_file,
            commands::files::get_file_version,
            commands::files::get_directory_fingerprint,
            commands::files::resolve_editor_file_reference,
            commands::files::write_file,
            commands::files::create_file,
            commands::files::create_dir,
            commands::files::rename_path,
            commands::files::delete_path,
            commands::files::reveal_path,
            commands::files::open_containing_directory,
            commands::files::open_path_with_default_app,
            commands::files::open_path_with_text_editor,
            commands::files::save_file_as,
            commands::files::read_text_file_for_clipboard,
            commands::files::get_default_file_open_target,
            commands::files::set_default_file_open_target,
            commands::git::watch_git_repo,
            commands::engines::get_execution_target,
            commands::engines::list_actived_clis,
            commands::engines::get_engine_info,
            commands::engines::get_chat_provider_usage,
            commands::engines::get_cli_context_usage,
            commands::engines::codex_uses_external_sandbox,
            commands::engines::engine_health,
            commands::engines::prewarm_engine,
            commands::engines::list_codex_skills,
            commands::engines::list_codex_apps,
            commands::engines::list_codex_plugins,
            commands::engines::get_opencode_runtime_catalog,
            commands::engines::run_engine_check,
            commands::extensions::get_extension_catalog,
            commands::extensions::get_cli_extensions,
            commands::extensions::schedule_extension_catalog_workspace_refresh,
            commands::extensions::request_extension_catalog_refresh,
            commands::extensions::get_extension_details,
            commands::extensions::perform_extension_action,
            commands::threads::list_threads,
            commands::threads::list_archived_threads,
            commands::threads::update_thread,
            commands::threads::update_thread_runtime_selection,
            commands::threads::list_codex_remote_threads,
            commands::threads::attach_codex_remote_thread,
            commands::threads::list_opencode_remote_sessions,
            commands::threads::attach_opencode_remote_session,
            commands::threads::create_thread,
            commands::threads::reconfigure_unstarted_thread_runtime,
            commands::threads::rename_thread,
            commands::threads::set_thread_reasoning_effort,
            commands::threads::set_ssh_remote_thread_selected_model,
            commands::threads::set_thread_execution_policy,
            commands::threads::get_thread_permissions,
            commands::threads::set_thread_permissions,
            commands::threads::set_thread_codex_config,
            commands::threads::set_thread_opencode_config,
            commands::threads::archive_thread,
            commands::threads::archive_thread_locally,
            commands::threads::restore_thread,
            commands::threads::sync_thread_from_engine,
            commands::threads::fork_codex_thread,
            commands::threads::rollback_codex_thread,
            commands::threads::compact_codex_thread,
            commands::threads::delete_thread,
            commands::terminal::terminal_create_session,
            commands::terminal::terminal_write,
            commands::terminal::terminal_write_bytes,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close_session,
            commands::terminal::terminal_close_workspace_sessions,
            commands::terminal::terminal_list_sessions,
            commands::terminal::terminal_get_renderer_diagnostics,
            commands::terminal::terminal_resume_session,
            commands::terminal::terminal_drain_output,
            commands::terminal::terminal_list_notifications,
            commands::terminal::terminal_clear_notification,
            commands::terminal::terminal_set_notification_focus,
            commands::setup::check_dependencies,
            commands::setup::install_dependency,
            commands::harness::check_harnesses,
            commands::harness::install_harness,
            commands::harness::launch_harness,
            commands::harness::get_harness_launch_args,
            commands::harness::set_harness_launch_args,
            cli_service_health::refresh_local_cli_health,
        ])
        // .build(tauri::generate_context!())
        .build(tauri_context)
        .expect("error while building tauri application");

    app.run(|app_handle, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => {
            if let Some(main_window) = app_handle.get_webview_window("main") {
                show_main_window(&main_window);
            }
        }
        RunEvent::ExitRequested { .. } | RunEvent::Exit => {
            let terminals = app_handle.state::<AppState>().terminals.clone();
            let keep_awake = app_handle.state::<AppState>().keep_awake.clone();
            let remote_access = app_handle.state::<AppState>().remote_access.clone();
            let computer_control_service = app_handle
                .state::<AppState>()
                .computer_control_service
                .clone();
            let mcp_gateway = app_handle.state::<AppState>().mcp_gateway.clone();
            let computer_control_sdk = app_handle
                .state::<Arc<computer_control_sdk::CuaDriverSdk>>()
                .inner()
                .clone();
            let ssh_monitor = app_handle.state::<AppState>().ssh_monitor.clone();
            tauri::async_runtime::block_on(async move {
                if let Err(error) = keep_awake.shutdown().await {
                    log::warn!("failed to release keep awake on shutdown: {error}");
                }
                terminals.shutdown().await;
                if let Err(error) =
                    local_cli_service_lifecycle::LocalCliServiceLifecycle::terminate_all().await
                {
                    log::warn!("关闭 AuraCoder 时停止本地 CLI 服务失败: {error:#}");
                }
                if let Err(error) = ssh::cli_service_lifecycle::terminate_all().await {
                    log::warn!("关闭 AuraCoder 时停止 SSH 远端 CLI 服务失败: {error:#}");
                }
                ssh::cli_tunnel_registry::shutdown().await;
                mcp_gateway.shutdown().await;
                computer_control_service.revoke_all().await;
                if let Err(error) = computer_control_sdk.shutdown() {
                    log::warn!("failed to shut down CUA SDK runtime: {error}");
                }
                remote_access.shutdown().await;
                ssh_monitor.shutdown().await;
            });
        }
        _ => {}
    });
}

fn show_database_version_error(
    tauri_context: tauri::Context<tauri::Wry>,
    current_version: u64,
    supported_version: u64,
) {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // .build(tauri::generate_context!())
        .build(tauri_context)
        .expect("failed to build database version error dialog");
    app.dialog()
        .message(format!(
            "数据库版本不符。\n\n当前数据库版本：{current_version}\n当前程序支持的数据库版本：{supported_version}"
        ))
        .title("AuraCoder")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .blocking_show();
}

fn create_main_window(app: &tauri::AppHandle) -> anyhow::Result<WebviewWindow> {
    let main_window_config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .or_else(|| app.config().app.windows.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("main window config not found"))?;

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let main_window_config = {
        let mut main_window_config = main_window_config;
        main_window_config.decorations = false;
        main_window_config
    };

    let main_window = WebviewWindowBuilder::from_config(app, &main_window_config)?
        .enable_clipboard_access()
        .build()?;

    let close_window = main_window.clone();
    main_window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Err(error) = close_window.hide() {
                log::warn!("failed to hide main window on close: {error}");
            }
        }
    });

    Ok(main_window)
}

fn build_system_tray(
    app: &tauri::App,
    locale: &str,
    main_window: WebviewWindow,
) -> tauri::Result<()> {
    let (open_label, quit_label) = tray_menu_strings(locale);
    let open_item = MenuItem::with_id(app, "tray-open", open_label, true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "tray-quit", quit_label, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &quit_item])?;
    let menu_window = main_window.clone();
    let tray_click_window = main_window;
    let exit_confirmation = scheduled_exit_confirmation_strings(locale);
    let exit_confirmation_open = Arc::new(AtomicBool::new(false));
    let menu_exit_confirmation_open = Arc::clone(&exit_confirmation_open);

    let mut builder = TrayIconBuilder::new()
        .tooltip("AuraCoder")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "tray-open" => show_main_window(&menu_window),
            "tray-quit" => request_tray_exit(app, exit_confirmation, &menu_exit_confirmation_open),
            _ => {}
        })
        .on_tray_icon_event(move |_tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => {
                show_main_window(&tray_click_window);
            }
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn request_tray_exit(
    app: &tauri::AppHandle,
    strings: ScheduledExitConfirmationStrings,
    confirmation_open: &Arc<AtomicBool>,
) {
    let has_enabled_tasks =
        match db::scheduled_tasks::has_tasks_in_enabled_column(&app.state::<AppState>().db) {
            Ok(has_enabled_tasks) => has_enabled_tasks,
            Err(error) => {
                log::warn!("failed to check enabled scheduled tasks before exit: {error}");
                true
            }
        };
    if !has_enabled_tasks {
        app.exit(0);
        return;
    }
    if confirmation_open.swap(true, Ordering::SeqCst) {
        return;
    }

    let app_handle = app.clone();
    let confirmation_open = Arc::clone(confirmation_open);
    app.dialog()
        .message(strings.message)
        .title(strings.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            strings.confirm.to_string(),
            strings.cancel.to_string(),
        ))
        .show(move |confirmed| {
            confirmation_open.store(false, Ordering::SeqCst);
            if confirmed {
                app_handle.exit(0);
            }
        });
}

fn show_main_window(window: &WebviewWindow) {
    if let Err(error) = window.show() {
        log::error!("failed to show main window: {error}");
        return;
    }
    if let Err(error) = window.unminimize() {
        log::warn!("failed to restore main window: {error}");
    }
    if let Err(error) = window.set_focus() {
        log::warn!("failed to focus main window: {error}");
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ThreadUpdatedEvent {
    thread_id: String,
    workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<ThreadDto>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRemoteThreadRemovedEvent {
    thread: ThreadDto,
    remote_action: &'static str,
}

async fn run_codex_runtime_bridge(app: tauri::AppHandle, state: AppState) {
    let codex = CliToolFactory::new(state.clone())
        .create("codex")
        .expect("Codex CLI factory mapping must exist");
    let cli: &dyn CliTool = codex.as_ref();
    let mut rx = cli.subscribe_codex_runtime_events();
    loop {
        match rx.recv().await {
            Ok(event) => handle_codex_runtime_event(&app, &state, event).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                log::warn!("codex runtime bridge lagged and skipped {skipped} events");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn handle_codex_runtime_event(
    app: &tauri::AppHandle,
    state: &AppState,
    event: CodexRuntimeEvent,
) {
    match event {
        CodexRuntimeEvent::DiagnosticsUpdated { diagnostics, toast } => {
            let _ = app.emit(
                "engine-runtime-updated",
                EngineRuntimeUpdatedDto {
                    engine_id: "codex".to_string(),
                    protocol_diagnostics: Some(diagnostics),
                    toast,
                },
            );
        }
        CodexRuntimeEvent::ApprovalResolved { approval_id } => {
            resolve_codex_runtime_approval(app, state, &approval_id).await;
        }
        CodexRuntimeEvent::ThreadStatusChanged {
            engine_thread_id,
            status_type,
            active_flags,
        } => {
            if let Some(updated_thread) = apply_codex_runtime_thread_update(
                state,
                &engine_thread_id,
                None,
                Some(status_type.as_str()),
                &active_flags,
                None,
                None,
                None,
            )
            .await
            {
                let _ = app.emit(
                    "thread-updated",
                    ThreadUpdatedEvent {
                        thread_id: updated_thread.id.clone(),
                        workspace_id: updated_thread.workspace_id.clone(),
                        thread: Some(updated_thread),
                    },
                );
            }
        }
        CodexRuntimeEvent::ThreadNameUpdated {
            engine_thread_id,
            thread_name,
        } => {
            let normalized_thread_name = thread_name.and_then(|name| {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            });
            let sync_required = if normalized_thread_name.is_some() {
                Some(false)
            } else {
                Some(true)
            };
            let sync_reason = if normalized_thread_name.is_some() {
                None
            } else {
                Some("thread_name_updated")
            };
            if let Some(updated_thread) = apply_codex_runtime_thread_update(
                state,
                &engine_thread_id,
                normalized_thread_name.as_deref(),
                None,
                &[],
                None,
                sync_required,
                sync_reason,
            )
            .await
            {
                let _ = app.emit(
                    "thread-updated",
                    ThreadUpdatedEvent {
                        thread_id: updated_thread.id.clone(),
                        workspace_id: updated_thread.workspace_id.clone(),
                        thread: Some(updated_thread),
                    },
                );
            }
        }
        CodexRuntimeEvent::ThreadSnapshotUpdated {
            engine_thread_id,
            thread_name,
            status_type,
            active_flags,
            preview,
        } => {
            if let Some(updated_thread) = apply_codex_runtime_thread_update(
                state,
                &engine_thread_id,
                thread_name.as_deref(),
                status_type.as_deref(),
                &active_flags,
                preview.as_deref(),
                Some(false),
                None,
            )
            .await
            {
                let _ = app.emit(
                    "thread-updated",
                    ThreadUpdatedEvent {
                        thread_id: updated_thread.id.clone(),
                        workspace_id: updated_thread.workspace_id.clone(),
                        thread: Some(updated_thread),
                    },
                );
            }
        }
        CodexRuntimeEvent::ThreadArchived { engine_thread_id } => {
            emit_codex_remote_thread_removed(app, state, &engine_thread_id, "archived").await;
        }
        CodexRuntimeEvent::ThreadDeleted { engine_thread_id } => {
            emit_codex_remote_thread_removed(app, state, &engine_thread_id, "deleted").await;
        }
        CodexRuntimeEvent::ThreadUnarchived { engine_thread_id } => {
            if let Some(updated_thread) =
                restore_codex_runtime_thread(state, &engine_thread_id).await
            {
                let _ = app.emit(
                    "thread-updated",
                    ThreadUpdatedEvent {
                        thread_id: updated_thread.id.clone(),
                        workspace_id: updated_thread.workspace_id.clone(),
                        thread: Some(updated_thread),
                    },
                );
            }
        }
    }
}

async fn resolve_codex_runtime_approval(
    app: &tauri::AppHandle,
    state: &AppState,
    approval_id: &str,
) {
    let Some((thread_id, message_id)) = run_db(state.db.clone(), {
        let approval_id = approval_id.to_string();
        move |db| db::actions::find_approval_context(db, &approval_id)
    })
    .await
    .ok()
    .flatten() else {
        return;
    };

    let has_local_turn = state.turns.get(&thread_id).await.is_some();
    let updated_thread = match run_db(state.db.clone(), {
        let approval_id = approval_id.to_string();
        let thread_id = thread_id.clone();
        let message_id = message_id.clone();
        move |db| {
            let mut conn = db.connect()?;
            let answered_at = runtime_env::system_time_rfc3339();
            let tx = conn
                .transaction()
                .context("failed to start approval resolution transaction")?;

            // Resolve the approval record.
            tx.execute(
                "UPDATE approvals
                 SET status = 'answered', answered_at = COALESCE(answered_at, ?1)
                 WHERE id = ?2",
                rusqlite::params![answered_at, approval_id],
            )
            .context("failed to resolve approval")?;

            // Update the approval block inside the message's blocks_json (best-effort).
            let raw_blocks: Option<String> = tx
                .query_row(
                    "SELECT blocks_json FROM messages WHERE id = ?1",
                    rusqlite::params![message_id],
                    |row| row.get(0),
                )
                .optional()
                .context("failed to load message blocks for approval update")?;
            if let Some(raw_blocks) = raw_blocks {
                let mut blocks_value: serde_json::Value =
                    serde_json::from_str(&raw_blocks).unwrap_or_else(|_| serde_json::json!([]));
                let changed = if let Some(items) = blocks_value.as_array_mut() {
                    let mut any_changed = false;
                    for block in items.iter_mut() {
                        let Some(object) = block.as_object_mut() else {
                            continue;
                        };
                        if object.get("type").and_then(serde_json::Value::as_str)
                            != Some("approval")
                        {
                            continue;
                        }
                        let bid = object
                            .get("approvalId")
                            .and_then(serde_json::Value::as_str)
                            .or_else(|| {
                                object
                                    .get("approval_id")
                                    .and_then(serde_json::Value::as_str)
                            });
                        if bid != Some(approval_id.as_str()) {
                            continue;
                        }
                        let should_update = object
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .map(|v| v != "answered")
                            .unwrap_or(true);
                        if should_update {
                            object.insert(
                                "status".to_string(),
                                serde_json::Value::String("answered".to_string()),
                            );
                            any_changed = true;
                        }
                    }
                    any_changed
                } else {
                    false
                };
                if changed {
                    tx.execute(
                        "UPDATE messages SET blocks_json = ?1 WHERE id = ?2",
                        rusqlite::params![blocks_value.to_string(), message_id],
                    )
                    .context("failed to persist answered approval in message blocks")?;
                }
            }

            // Conditionally advance the thread status.
            if has_local_turn {
                tx.execute(
                    "UPDATE threads
                     SET status = ?1, last_activity_at = ?2
                     WHERE id = ?3
                       AND status = ?4",
                    rusqlite::params![
                        ThreadStatusDto::Streaming.as_str(),
                        runtime_env::system_time_rfc3339(),
                        thread_id,
                        ThreadStatusDto::AwaitingApproval.as_str()
                    ],
                )
                .context("failed to conditionally update thread status")?;
            }

            tx.commit()
                .context("failed to commit approval resolution transaction")?;

            // Read the updated thread after the transaction has committed (non-atomic read is fine).
            let updated_thread = if has_local_turn {
                db::threads::get_thread(db, &thread_id)?
            } else {
                None
            };

            Ok(updated_thread)
        }
    })
    .await
    {
        Ok(updated_thread) => updated_thread,
        Err(error) => {
            log::warn!("failed to reconcile resolved runtime approval {approval_id}: {error}");
            return;
        }
    };

    let stream_event_topic = format!("stream-event-{thread_id}");
    let _ = app.emit(
        &stream_event_topic,
        serde_json::json!({
            "type": "ApprovalResolved",
            "approval_id": approval_id,
        }),
    );

    if let Some(thread) = updated_thread {
        let _ = app.emit(
            "thread-updated",
            ThreadUpdatedEvent {
                thread_id: thread.id.clone(),
                workspace_id: thread.workspace_id.clone(),
                thread: Some(thread),
            },
        );
    }
}

async fn apply_codex_runtime_thread_update(
    state: &AppState,
    engine_thread_id: &str,
    title: Option<&str>,
    raw_status: Option<&str>,
    active_flags: &[String],
    preview: Option<&str>,
    sync_required: Option<bool>,
    sync_reason: Option<&str>,
) -> Option<ThreadDto> {
    let thread = run_db(state.db.clone(), {
        let engine_thread_id = engine_thread_id.to_string();
        move |db| db::threads::find_thread_by_engine_thread_id(db, "codex", &engine_thread_id)
    })
    .await
    .ok()??;

    let has_local_turn = state.turns.get(&thread.id).await.is_some();
    let next_status = map_codex_runtime_status_to_local(raw_status, active_flags, has_local_turn);
    let metadata = merge_codex_runtime_metadata(
        thread.engine_metadata.clone(),
        raw_status,
        active_flags,
        preview,
        sync_required,
        sync_reason,
    );

    run_db(state.db.clone(), {
        let thread_id = thread.id.clone();
        let title = title.map(str::to_string);
        let metadata = metadata.clone();
        let next_status = next_status.clone();
        move |db| {
            db::threads::update_thread_runtime_snapshot(
                db,
                &thread_id,
                title.as_deref(),
                next_status,
                Some(&metadata),
            )
        }
    })
    .await
    .ok()
}

async fn emit_codex_remote_thread_removed(
    app: &tauri::AppHandle,
    state: &AppState,
    engine_thread_id: &str,
    remote_action: &'static str,
) {
    let Some(thread) = find_active_codex_runtime_thread(state, engine_thread_id).await else {
        return;
    };

    let _ = app.emit(
        "codex-remote-thread-removed",
        CodexRemoteThreadRemovedEvent {
            thread,
            remote_action,
        },
    );
}

async fn find_active_codex_runtime_thread(
    state: &AppState,
    engine_thread_id: &str,
) -> Option<ThreadDto> {
    run_db(state.db.clone(), {
        let engine_thread_id = engine_thread_id.to_string();
        move |db| {
            let Some(thread) =
                db::threads::find_thread_by_engine_thread_id(db, "codex", &engine_thread_id)?
            else {
                return Ok::<Option<ThreadDto>, anyhow::Error>(None);
            };
            let active = db::threads::list_threads_for_workspace(db, &thread.workspace_id)?
                .into_iter()
                .any(|candidate| candidate.id == thread.id);
            Ok(active.then_some(thread))
        }
    })
    .await
    .ok()
    .flatten()
}

async fn restore_codex_runtime_thread(
    state: &AppState,
    engine_thread_id: &str,
) -> Option<ThreadDto> {
    let thread = run_db(state.db.clone(), {
        let engine_thread_id = engine_thread_id.to_string();
        move |db| db::threads::find_thread_by_engine_thread_id(db, "codex", &engine_thread_id)
    })
    .await
    .ok()??;

    run_db(state.db.clone(), {
        let thread_id = thread.id.clone();
        let existing = thread.clone();
        move |db| match db::threads::restore_thread(db, &thread_id) {
            Ok(restored) => Ok(restored),
            Err(error) if error.to_string().contains("not archived") => Ok(existing),
            Err(error) => Err(error),
        }
    })
    .await
    .ok()
}

fn merge_codex_runtime_metadata(
    existing: Option<serde_json::Value>,
    raw_status: Option<&str>,
    active_flags: &[String],
    preview: Option<&str>,
    sync_required: Option<bool>,
    sync_reason: Option<&str>,
) -> serde_json::Value {
    let mut metadata = existing.unwrap_or_else(|| serde_json::json!({}));
    if !metadata.is_object() {
        metadata = serde_json::json!({});
    }

    if let Some(object) = metadata.as_object_mut() {
        if raw_status.is_some() {
            match raw_status.map(str::trim).filter(|value| !value.is_empty()) {
                Some(status) => {
                    object.insert("codexThreadStatus".to_string(), serde_json::json!(status));
                }
                None => {
                    object.remove("codexThreadStatus");
                }
            }

            if active_flags.is_empty() {
                object.remove("codexThreadActiveFlags");
            } else {
                object.insert(
                    "codexThreadActiveFlags".to_string(),
                    serde_json::json!(active_flags),
                );
            }
        }

        if preview.is_some() {
            match preview.map(str::trim).filter(|value| !value.is_empty()) {
                Some(preview) => {
                    object.insert("codexPreview".to_string(), serde_json::json!(preview));
                }
                None => {
                    object.remove("codexPreview");
                }
            }
        }

        if let Some(sync_required) = sync_required {
            object.insert(
                "codexSyncRequired".to_string(),
                serde_json::json!(sync_required),
            );
            object.insert(
                "codexSyncUpdatedAt".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            );
            match sync_reason.map(str::trim).filter(|value| !value.is_empty()) {
                Some(reason) => {
                    object.insert("codexSyncReason".to_string(), serde_json::json!(reason));
                }
                None => {
                    object.insert("codexSyncReason".to_string(), serde_json::Value::Null);
                }
            }
        }
    }

    metadata
}

fn map_codex_runtime_status_to_local(
    raw_status: Option<&str>,
    active_flags: &[String],
    has_local_turn: bool,
) -> Option<ThreadStatusDto> {
    if has_local_turn {
        return None;
    }

    match raw_status.map(str::trim).filter(|value| !value.is_empty()) {
        Some("systemError") => Some(ThreadStatusDto::Error),
        Some("idle") | Some("notLoaded") => Some(ThreadStatusDto::Idle),
        Some("active") => {
            if active_flags
                .iter()
                .any(|flag| matches!(flag.as_str(), "waitingOnApproval" | "waitingOnUserInput"))
            {
                Some(ThreadStatusDto::AwaitingApproval)
            } else {
                Some(ThreadStatusDto::Streaming)
            }
        }
        _ => None,
    }
}

async fn run_db<T, F>(db: crate::db::Database, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Database) -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

fn build_app_menu(handle: &tauri::AppHandle, locale: &str) -> tauri::Result<Menu<tauri::Wry>> {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let _ = locale;
        return Menu::with_items(handle, &[]);
    }

    #[cfg(target_os = "macos")]
    {
        let strings = native_strings(locale);

        let app_menu = SubmenuBuilder::new(handle, strings.app_menu)
            .about(Some(AboutMetadata {
                name: Some("AuraCoder".to_string()),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
                authors: Some(vec!["Wygor Alves".to_string()]),
                comments: Some(strings.about_comments.to_string()),
                copyright: Some("Copyright © 2026 Wygor Alves".to_string()),
                license: Some("MIT".to_string()),
                website: Some("https://github.com/wygoralves/auracoder".to_string()),
                website_label: Some("GitHub".to_string()),
                icon: match Image::from_bytes(include_bytes!("../icons/128x128@2x.png")) {
                    Ok(img) => Some(img),
                    Err(e) => {
                        log::warn!("failed to load about icon: {e}");
                        None
                    }
                },
                ..Default::default()
            }))
            .separator()
            .item(&PredefinedMenuItem::services(handle, None)?)
            .separator()
            .hide()
            .hide_others()
            .show_all()
            .separator()
            .quit()
            .build()?;

        let edit_menu = SubmenuBuilder::new(handle, strings.edit_menu)
            .undo()
            .redo()
            .separator()
            .cut()
            .copy()
            .paste()
            .select_all()
            .build()?;

        let toggle_sidebar = MenuItem::with_id(
            handle,
            "toggle-sidebar",
            strings.toggle_sidebar,
            true,
            Some("CmdOrCtrl+B"),
        )?;
        let toggle_git_panel = MenuItem::with_id(
            handle,
            "toggle-git-panel",
            strings.toggle_git_panel,
            true,
            Some("CmdOrCtrl+Shift+B"),
        )?;
        let toggle_focus_mode = MenuItem::with_id(
            handle,
            "toggle-focus-mode",
            strings.toggle_focus_mode,
            true,
            Some("CmdOrCtrl+Alt+F"),
        )?;
        let toggle_fullscreen = MenuItem::with_id(
            handle,
            "toggle-fullscreen",
            strings.toggle_fullscreen,
            true,
            Some("F11"),
        )?;
        let toggle_search = MenuItem::with_id(
            handle,
            "toggle-search",
            strings.search,
            true,
            Some("CmdOrCtrl+Shift+F"),
        )?;
        let toggle_terminal = MenuItem::with_id(
            handle,
            "toggle-terminal",
            strings.toggle_terminal,
            true,
            Some("CmdOrCtrl+Shift+T"),
        )?;
        let view_menu = SubmenuBuilder::new(handle, strings.view_menu)
            .item(&toggle_sidebar)
            .item(&toggle_git_panel)
            .item(&toggle_focus_mode)
            .item(&toggle_fullscreen)
            .separator()
            .item(&toggle_search)
            .separator()
            .item(&toggle_terminal)
            .build()?;

        let close_window = MenuItem::with_id(
            handle,
            "close-window",
            strings.close,
            true,
            Some("CmdOrCtrl+W"),
        )?;
        let window_menu = SubmenuBuilder::new(handle, strings.window_menu)
            .minimize()
            .item(&PredefinedMenuItem::maximize(handle, None)?)
            .separator()
            .item(&close_window)
            .build()?;

        return Menu::with_items(handle, &[&app_menu, &edit_menu, &view_menu, &window_menu]);
    }

    #[allow(unreachable_code)]
    Menu::with_items(handle, &[])
}
