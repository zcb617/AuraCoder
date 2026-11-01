use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(target_os = "windows")]
use std::collections::BTreeSet;

use anyhow::Context;

use crate::services::{app_config_service::AppConfigService, workspace_service::WorkspaceService};
use crate::{
    fs_ops,
    models::{
        DefaultFileOpenTargetDto, FileTreeEntryDto, ReadFileResultDto,
        ResolvedEditorFileReferenceDto, TextEditorApplicationDto, WorkspaceTarget,
        WriteFileResultDto,
    },
    path_utils,
    ssh::{remote_fs, runtime::{remote_workspace_marker, workspace_id_from_workspace_marker}},
    state::AppState,
};

/// 文件命令的公共业务处理与平台文件操作入口。
pub struct FileService {
    /// 供文件业务读取工作区、缓存和配置锁的应用状态。
    state: AppState,
}

impl FileService {
        /// 创建绑定应用状态的文件业务 Service。
        pub fn new(state: AppState) -> Self { Self { state } }

        /// 读取工作区目录内容。
        pub async fn list_dir(
        &self,
        root_path: String,
        dir_path: String,
        workspace_id: Option<String>,
    ) -> Result<Vec<FileTreeEntryDto>, String> {

        // 以下代码实现读取工作区目录内容。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::list_dir(connection, &root, &dir_path)
                    .await
                    .map_err(err_to_string);
            }
        }
        tokio::task::spawn_blocking(move || {
            fs_ops::list_dir(&root_path, &dir_path).map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 读取工作区文件内容。
        pub async fn read_file(
        &self,
        root_path: String,
        file_path: String,
        workspace_id: Option<String>,
    ) -> Result<ReadFileResultDto, String> {

        // 以下代码实现读取工作区文件内容。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::read_file(connection, &root, &file_path)
                    .await
                    .map_err(err_to_string);
            }
        }
        tokio::task::spawn_blocking(move || {
            fs_ops::read_file(&root_path, &file_path).map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 获取文件版本以校验并发写入。
        pub async fn get_file_version(
        &self,
        root_path: String,
        file_path: String,
        workspace_id: Option<String>,
    ) -> Result<String, String> {

        // 以下代码实现获取文件版本以校验并发写入。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::file_version(connection, &root, &file_path)
                    .await
                    .map_err(err_to_string);
            }
        }
        tokio::task::spawn_blocking(move || {
            fs_ops::read_file(&root_path, &file_path)
                .map(|result| result.version)
                .map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 读取远程目录变更指纹。
        pub async fn get_directory_fingerprint(
        &self,
        root_path: String,
        dir_path: String,
        workspace_id: Option<String>,
    ) -> Result<String, String> {

        // 以下代码实现读取远程目录变更指纹。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::directory_fingerprint(connection, &root, &dir_path)
                    .await
                    .map_err(err_to_string);
            }
        }
        Err(format!("本地目录使用文件监听，不需要目录指纹：{root_path}"))

        }
        /// 解析工作区内的编辑器文件引用。
        pub async fn resolve_editor_file_reference(
        &self,
        workspace_id: String,
        raw_reference: String,
    ) -> Result<Option<ResolvedEditorFileReferenceDto>, String> {

        // 以下代码实现解析工作区内的编辑器文件引用。
        let state = &self.state;
        let target = load_workspace_target(state, &workspace_id).await?;
        if target.is_remote() {
            let Some(parsed) = parse_editor_file_reference(&raw_reference) else {
                return Ok(None);
            };
            let file_path = parsed.path.to_string_lossy().replace('\\', "/");
            crate::ssh::runtime::validate_remote_relative_path(&file_path, false)
                .map_err(err_to_string)?;
            let connection = target.remote_connection().map_err(err_to_string)?;
            remote_fs::read_file(connection, &target.workspace.root_path, &file_path)
                .await
                .map_err(err_to_string)?;
            return Ok(Some(ResolvedEditorFileReferenceDto {
                root_path: target.workspace.root_path.clone(),
                file_path,
                line: parsed.line,
                column: parsed.column,
            }));
        }
        let workspace = WorkspaceService::new(state.clone())
            .get_workspace(&workspace_id)
            .await
            .map_err(err_to_string)?;
        tokio::task::spawn_blocking(move || {
            resolve_editor_file_reference_impl(&workspace.root_path, &raw_reference)
                .map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 写入文件并刷新文件树缓存。
        pub async fn write_file(
        &self,
        root_path: String,
        file_path: String,
        content: String,
        workspace_id: Option<String>,
        expected_version: Option<String>,
    ) -> Result<WriteFileResultDto, String> {

        // 以下代码实现写入文件并刷新文件树缓存。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::write_file(
                    connection,
                    &root,
                    &file_path,
                    &content,
                    expected_version.as_deref(),
                )
                .await
                .map_err(err_to_string);
            }
        }
        let cache = state.file_tree_cache.clone();
        tokio::task::spawn_blocking(move || {
            let access_root = PathBuf::from(&root_path)
                .canonicalize()
                .map_err(err_to_string)?;
            let target_for_repo_lookup =
                resolve_target_path_for_repo_lookup(&access_root, &file_path).map_err(err_to_string)?;

            let result = fs_ops::write_file(
                &root_path,
                &file_path,
                &content,
                expected_version.as_deref(),
            )
            .map_err(err_to_string)?;
            cache.invalidate_containing_path(target_for_repo_lookup.to_string_lossy().as_ref());
            Ok(result)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 创建文件并刷新文件树缓存。
        pub async fn create_file(
        &self,
        root_path: String,
        file_path: String,
        workspace_id: Option<String>,
    ) -> Result<(), String> {

        // 以下代码实现创建文件并刷新文件树缓存。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::create_file(connection, &root, &file_path)
                    .await
                    .map_err(err_to_string);
            }
        }
        let cache = state.file_tree_cache.clone();
        tokio::task::spawn_blocking(move || {
            let access_root = PathBuf::from(&root_path)
                .canonicalize()
                .map_err(err_to_string)?;
            let target_for_repo_lookup =
                resolve_target_path_for_repo_lookup(&access_root, &file_path).map_err(err_to_string)?;

            fs_ops::create_file(&root_path, &file_path).map_err(err_to_string)?;
            cache.invalidate_containing_path(target_for_repo_lookup.to_string_lossy().as_ref());
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 创建目录并刷新文件树缓存。
        pub async fn create_dir(
        &self,
        root_path: String,
        dir_path: String,
        workspace_id: Option<String>,
    ) -> Result<(), String> {

        // 以下代码实现创建目录并刷新文件树缓存。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::create_dir(connection, &root, &dir_path)
                    .await
                    .map_err(err_to_string);
            }
        }
        let cache = state.file_tree_cache.clone();
        tokio::task::spawn_blocking(move || {
            let access_root = PathBuf::from(&root_path)
                .canonicalize()
                .map_err(err_to_string)?;
            let target_for_repo_lookup =
                resolve_target_path_for_repo_lookup(&access_root, &dir_path).map_err(err_to_string)?;

            fs_ops::create_dir(&root_path, &dir_path).map_err(err_to_string)?;
            cache.invalidate_containing_path(target_for_repo_lookup.to_string_lossy().as_ref());
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 重命名路径并刷新文件树缓存。
        pub async fn rename_path(
        &self,
        root_path: String,
        old_path: String,
        new_name: String,
        workspace_id: Option<String>,
    ) -> Result<(), String> {

        // 以下代码实现重命名路径并刷新文件树缓存。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::rename_path(connection, &root, &old_path, &new_name)
                    .await
                    .map_err(err_to_string);
            }
        }
        let cache = state.file_tree_cache.clone();
        tokio::task::spawn_blocking(move || {
            let access_root = PathBuf::from(&root_path)
                .canonicalize()
                .map_err(err_to_string)?;
            let target_for_repo_lookup =
                resolve_target_path_for_repo_lookup(&access_root, &old_path).map_err(err_to_string)?;

            fs_ops::rename_path(&root_path, &old_path, &new_name).map_err(err_to_string)?;
            cache.invalidate_containing_path(target_for_repo_lookup.to_string_lossy().as_ref());
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 删除路径并刷新文件树缓存。
        pub async fn delete_path(
        &self,
        root_path: String,
        file_path: String,
        workspace_id: Option<String>,
    ) -> Result<(), String> {

        // 以下代码实现删除路径并刷新文件树缓存。
        let state = &self.state;
        if let Some(workspace_id) = workspace_id.as_deref() {
            let target = load_workspace_target(state, workspace_id).await?;
            if target.is_remote() {
                let connection = target.remote_connection().map_err(err_to_string)?;
                let root = remote_access_root(&target, &root_path).await?;
                return remote_fs::delete_path(connection, &root, &file_path)
                    .await
                    .map_err(err_to_string);
            }
        }
        let cache = state.file_tree_cache.clone();
        tokio::task::spawn_blocking(move || {
            let access_root = PathBuf::from(&root_path)
                .canonicalize()
                .map_err(err_to_string)?;
            let target_for_repo_lookup =
                resolve_target_path_for_repo_lookup(&access_root, &file_path).map_err(err_to_string)?;

            fs_ops::delete_path(&root_path, &file_path).map_err(err_to_string)?;
            cache.invalidate_containing_path(target_for_repo_lookup.to_string_lossy().as_ref());
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 在系统文件管理器定位路径。
        pub async fn reveal_path(path: String) -> Result<(), String> {

        // 以下代码实现在系统文件管理器定位路径。
        tokio::task::spawn_blocking(move || {
            reveal_path_impl(PathBuf::from(path)).map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 打开指定路径所属目录。
        pub async fn open_containing_directory(path: String) -> Result<(), String> {

        // 以下代码实现打开指定路径所属目录。
        tokio::task::spawn_blocking(move || {
            open_containing_directory_impl(PathBuf::from(path)).map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 使用配置的默认应用打开路径。
        pub async fn open_path_with_default_app(path: String) -> Result<(), String> {

        // 以下代码实现使用配置的默认应用打开路径。
        tokio::task::spawn_blocking(move || {
            open_path_with_default_app_impl(PathBuf::from(path)).map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 使用指定文本编辑器打开路径。
        pub async fn open_path_with_text_editor(
        path: String,
        editor_id: Option<String>,
    ) -> Result<(), String> {

        // 以下代码实现使用指定文本编辑器打开路径。
        tokio::task::spawn_blocking(move || {
            open_path_with_text_editor_impl(PathBuf::from(path), editor_id).map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 复制源文件到目标路径。
        pub async fn save_file_as(source_path: String, destination_path: String) -> Result<(), String> {

        // 以下代码实现复制源文件到目标路径。
        tokio::task::spawn_blocking(move || {
            save_file_as_impl(PathBuf::from(source_path), PathBuf::from(destination_path))
                .map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 读取可复制到剪贴板的文本文件。
        pub async fn read_text_file_for_clipboard(path: String) -> Result<Option<String>, String> {

        // 以下代码实现读取可复制到剪贴板的文本文件。
        tokio::task::spawn_blocking(move || {
            read_text_file_for_clipboard_impl(PathBuf::from(path)).map_err(err_to_string)
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 读取默认文件打开目标及编辑器列表。
        pub async fn get_default_file_open_target() -> Result<DefaultFileOpenTargetDto, String> {

        // 以下代码实现读取默认文件打开目标及编辑器列表。
        tokio::task::spawn_blocking(|| {
            // let selected_editor_id = AppConfig::load_or_create()
            let selected_editor_id = AppConfigService::load_or_create()
                .map_err(err_to_string)?
                .general
                .default_file_open_target;
            let applications = detect_text_editor_applications();
            let selected_editor_id = selected_editor_id.filter(|selected| {
                applications
                    .iter()
                    .any(|application| application.dto.id == *selected)
            });

            Ok(DefaultFileOpenTargetDto {
                selected_editor_id,
                applications: applications
                    .into_iter()
                    .map(|application| application.dto)
                    .collect(),
            })
        })
        .await
        .map_err(|error| error.to_string())?

        }
        /// 校验并保存默认文件打开目标。
        pub async fn set_default_file_open_target(
        &self,
        editor_id: Option<String>,
    ) -> Result<Option<String>, String> {

        // 以下代码实现校验并保存默认文件打开目标。
        let state = &self.state;
        let selected_editor_id = editor_id.filter(|value| !value.trim().is_empty());
        let config_write_lock = state.config_write_lock.clone();
        let _guard = config_write_lock.lock_owned().await;

        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<String>> {
            if let Some(selected) = selected_editor_id.as_deref() {
                let available = detect_text_editor_applications()
                    .iter()
                    .any(|application| application.dto.id == selected);
                anyhow::ensure!(available, "the selected text editor is no longer available");
            }

            // AppConfig::mutate(|config| {
            AppConfigService::mutate(|config| {
                config.general.default_file_open_target = selected_editor_id.clone();
                Ok(selected_editor_id)
            })
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(err_to_string)

        }
}
/// 保存编辑器展示信息与平台启动参数。
#[derive(Debug, Clone)]
struct TextEditorApplication {
    /// 编辑器对前端展示的标识和名称。
    dto: TextEditorApplicationDto,
    /// 当前平台对应的编辑器启动方式。
    launch: TextEditorLaunch,
}

/// 保存不同平台启动文本编辑器所需的数据。
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum TextEditorLaunch {
    #[cfg(target_os = "windows")]
    Windows {
        /// Windows 注册表命令解析出的程序。
        program: PathBuf,
    },
    #[cfg(target_os = "macos")]
    Macos {
        /// macOS 编辑器应用包路径。
        bundle_path: PathBuf,
    },
    #[cfg(target_os = "linux")]
    Linux {
        /// Linux 桌面入口文件路径。
        desktop_file: PathBuf,
        /// 桌面入口解析出的启动程序。
        program: OsString,
        /// 桌面入口解析出的启动参数。
        args: Vec<OsString>,
    },
    Unsupported,
}

/// 按当前平台探测可用文本编辑器。
fn detect_text_editor_applications() -> Vec<TextEditorApplication> {

    // 以下代码实现按当前平台探测可用文本编辑器。
    let mut applications = match reveal_platform() {
        RevealPlatform::Windows => {
            #[cfg(target_os = "windows")]
            {
                detect_windows_text_editor_applications()
            }
            #[cfg(not(target_os = "windows"))]
            {
                Vec::new()
            }
        }
        RevealPlatform::Macos => {
            #[cfg(target_os = "macos")]
            {
                detect_macos_text_editor_applications()
            }
            #[cfg(not(target_os = "macos"))]
            {
                Vec::new()
            }
        }
        RevealPlatform::Linux => {
            #[cfg(target_os = "linux")]
            {
                detect_linux_text_editor_applications()
            }
            #[cfg(not(target_os = "linux"))]
            {
                Vec::new()
            }
        }
        RevealPlatform::Unsupported => Vec::new(),
    };

    applications.sort_by(|left, right| {
        left.dto
            .name
            .to_lowercase()
            .cmp(&right.dto.name.to_lowercase())
            .then_with(|| left.dto.id.cmp(&right.dto.id))
    });
    applications.dedup_by(|left, right| left.dto.id == right.dto.id);
    applications

}

/// 判断文件类型是否属于可编辑文本。
fn is_text_document_type(value: &str) -> bool {

    // 以下代码实现判断文件类型是否属于可编辑文本。
    let value = value.trim().to_ascii_lowercase();
    value == ".txt"
        || value == "text/plain"
        || value.starts_with("text/")
        || value == "public.text"
        || value == "public.plain-text"
        || value.contains("source-code")

}

/// 按平台启动参数打开文本编辑器。
fn launch_text_editor(application: &TextEditorApplication, path: &Path) -> anyhow::Result<()> {

    // 以下代码实现按平台启动参数打开文本编辑器。
    let mut command = match &application.launch {
        #[cfg(target_os = "windows")]
        TextEditorLaunch::Windows { program } => Command::new(program),
        #[cfg(target_os = "macos")]
        TextEditorLaunch::Macos { bundle_path } => {
            let mut command = Command::new("open");
            command.arg("-a").arg(bundle_path);
            command
        }
        #[cfg(target_os = "linux")]
        TextEditorLaunch::Linux {
            desktop_file,
            program,
            args,
        } => {
            if let Some(gio) = crate::runtime_env::resolve_executable("gio") {
                let mut command = Command::new(gio);
                command.arg("launch").arg(desktop_file);
                command
            } else {
                let mut command = Command::new(program);
                command.args(args);
                command
            }
        }
        TextEditorLaunch::Unsupported => anyhow::bail!("text editor launch is not supported"),
    };
    command.arg(path);
    spawn_path_command(command, path, "open with the configured text editor")

}

#[cfg(target_os = "windows")]
/// 读取 Windows 中可用的文本编辑器。
fn detect_windows_text_editor_applications() -> Vec<TextEditorApplication> {

    // 以下代码实现读取 Windows 中可用的文本编辑器。
    use winreg::{
        enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
        RegKey,
    };

    let mut applications = BTreeMap::new();
    let mut text_program_ids = BTreeSet::new();
    let mut text_application_keys = BTreeSet::new();
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let root = RegKey::predef(hive);
        if let Ok(registered) = root.open_subkey("Software\\Classes\\Applications") {
            for application_key in registered.enum_keys().flatten() {
                let Ok(application) = registered.open_subkey(&application_key) else {
                    continue;
                };
                let supports_text =
                    application
                        .open_subkey("SupportedTypes")
                        .ok()
                        .is_some_and(|types| {
                            types
                                .enum_values()
                                .flatten()
                                .any(|(value_name, _)| is_text_document_type(&value_name))
                        });
                if !supports_text {
                    continue;
                }

                let Some(command) = application
                    .open_subkey("shell\\open\\command")
                    .ok()
                    .and_then(|key| key.get_value::<String, _>("").ok())
                else {
                    continue;
                };
                add_windows_command_application(&mut applications, command, &application_key);
            }
        }

        for path in [
            "Software\\Classes\\.txt\\OpenWithProgids",
            "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.txt\\OpenWithProgids",
        ] {
            if let Ok(registered) = root.open_subkey(path) {
                text_program_ids.extend(registered.enum_values().flatten().map(|(name, _)| name));
            }
        }
        if hive == HKEY_CURRENT_USER {
            if let Ok(registered) = root.open_subkey(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.txt\\OpenWithList",
            ) {
                for (value_name, _) in registered.enum_values().flatten() {
                    if value_name == "MRUList" {
                        continue;
                    }
                    if let Ok(application_key) = registered.get_value::<String, _>(&value_name) {
                        if application_key.to_ascii_lowercase().ends_with(".exe") {
                            text_application_keys.insert(application_key);
                        }
                    }
                }
            }
        }
    }

    for program_id in text_program_ids {
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let root = RegKey::predef(hive);
            let path = format!("Software\\Classes\\{program_id}\\shell\\open\\command");
            if let Ok(command) = root
                .open_subkey(path)
                .and_then(|key| key.get_value::<String, _>(""))
            {
                add_windows_command_application(&mut applications, command, &program_id);
            }
        }
    }

    for application_key in text_application_keys {
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let root = RegKey::predef(hive);
            let path =
                format!("Software\\Classes\\Applications\\{application_key}\\shell\\open\\command");
            if let Ok(command) = root
                .open_subkey(path)
                .and_then(|key| key.get_value::<String, _>(""))
            {
                add_windows_command_application(&mut applications, command, &application_key);
            }
        }
    }
    applications.into_values().collect()

}

#[cfg(target_os = "windows")]
/// 解析 Windows 命令并加入编辑器列表。
fn add_windows_command_application(
    applications: &mut BTreeMap<String, TextEditorApplication>,
    command: String,
    fallback_name: &str,
) {

    // 以下代码实现解析 Windows 命令并加入编辑器列表。
    let Some(program) = extract_windows_launch_program(&command) else {
        return;
    };
    if !program.is_file() {
        return;
    }

    let id = format!("windows:{}", program.to_string_lossy().to_ascii_lowercase());
    let name = program
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(fallback_name)
        .to_string();
    applications
        .entry(id.clone())
        .or_insert(TextEditorApplication {
            dto: TextEditorApplicationDto { id, name },
            launch: TextEditorLaunch::Windows { program },
        });

}

#[cfg(target_os = "windows")]
/// 从 Windows 打开命令提取程序路径。
fn extract_windows_launch_program(command: &str) -> Option<PathBuf> {

    // 以下代码实现从 Windows 打开命令提取程序路径。
    let expanded = expand_windows_environment_variables(command.trim());
    let candidate = if let Some(quoted) = expanded.strip_prefix('"') {
        quoted.split_once('"')?.0
    } else {
        let end = expanded.to_ascii_lowercase().find(".exe")? + ".exe".len();
        &expanded[..end]
    };
    let program = PathBuf::from(candidate.trim());
    program.is_absolute().then_some(program)

}

#[cfg(target_os = "windows")]
/// 展开 Windows 环境变量占位符。
fn expand_windows_environment_variables(value: &str) -> String {

    // 以下代码实现展开 Windows 环境变量占位符。
    let mut expanded = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find('%') {
        expanded.push_str(&remaining[..start]);
        let after_start = &remaining[start + 1..];
        let Some(end) = after_start.find('%') else {
            expanded.push('%');
            expanded.push_str(after_start);
            return expanded;
        };
        let variable = &after_start[..end];
        if variable.is_empty() {
            expanded.push('%');
        } else if let Some(replacement) = std::env::var_os(variable) {
            expanded.push_str(&replacement.to_string_lossy());
        } else {
            expanded.push('%');
            expanded.push_str(variable);
            expanded.push('%');
        }
        remaining = &after_start[end + 1..];
    }
    expanded.push_str(remaining);
    expanded

}

#[cfg(target_os = "macos")]
/// 探测 macOS 应用目录中的文本编辑器。
fn detect_macos_text_editor_applications() -> Vec<TextEditorApplication> {

    // 以下代码实现探测 macOS 应用目录中的文本编辑器。
    use plist::Value;

    let mut bundle_paths = Vec::new();
    for root in [
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Applications"))
            .unwrap_or_default(),
    ] {
        collect_macos_application_bundles(&root, 2, &mut bundle_paths);
    }

    let mut applications = BTreeMap::new();
    for bundle_path in bundle_paths {
        let info_path = bundle_path.join("Contents/Info.plist");
        let Ok(info) = Value::from_file(info_path) else {
            continue;
        };
        let Some(dictionary) = info.as_dictionary() else {
            continue;
        };
        let supports_text = dictionary
            .get("CFBundleDocumentTypes")
            .and_then(Value::as_array)
            .is_some_and(|document_types| {
                document_types.iter().any(|document_type| {
                    let Some(document_type) = document_type.as_dictionary() else {
                        return false;
                    };
                    [
                        "CFBundleTypeMIMETypes",
                        "LSItemContentTypes",
                        "CFBundleTypeExtensions",
                    ]
                    .iter()
                    .any(|key| {
                        document_type
                            .get(*key)
                            .and_then(Value::as_array)
                            .is_some_and(|values| {
                                values
                                    .iter()
                                    .filter_map(Value::as_string)
                                    .any(is_text_document_type)
                            })
                    })
                })
            });
        if !supports_text {
            continue;
        }

        let bundle_path = bundle_path.canonicalize().unwrap_or(bundle_path);
        let id = format!("macos:{}", bundle_path.to_string_lossy());
        let name = ["CFBundleDisplayName", "CFBundleName"]
            .iter()
            .find_map(|key| dictionary.get(*key).and_then(Value::as_string))
            .filter(|name| !name.trim().is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                bundle_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| "Application".to_string());
        applications
            .entry(id.clone())
            .or_insert(TextEditorApplication {
                dto: TextEditorApplicationDto { id, name },
                launch: TextEditorLaunch::Macos { bundle_path },
            });
    }
    applications.into_values().collect()

}

#[cfg(target_os = "macos")]
/// 递归收集 macOS 应用包路径。
fn collect_macos_application_bundles(directory: &Path, depth: usize, bundles: &mut Vec<PathBuf>) {

    // 以下代码实现递归收集 macOS 应用包路径。
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("app"))
        {
            bundles.push(path);
        } else if depth > 0 && path.is_dir() {
            collect_macos_application_bundles(&path, depth - 1, bundles);
        }
    }

}

#[cfg(target_os = "linux")]
/// 解析 Linux 桌面入口中的文本编辑器。
fn detect_linux_text_editor_applications() -> Vec<TextEditorApplication> {

    // 以下代码实现解析 Linux 桌面入口中的文本编辑器。
    let mut desktop_files = Vec::new();
    for root in linux_application_directories() {
        collect_linux_desktop_files(&root, 2, &mut desktop_files);
    }

    let mut applications = BTreeMap::new();
    for desktop_file in desktop_files {
        let Some((name, program, args)) = parse_linux_text_editor_desktop_file(&desktop_file)
        else {
            continue;
        };
        let desktop_file = desktop_file.canonicalize().unwrap_or(desktop_file);
        let id = format!("linux:{}", desktop_file.to_string_lossy());
        applications
            .entry(id.clone())
            .or_insert(TextEditorApplication {
                dto: TextEditorApplicationDto { id, name },
                launch: TextEditorLaunch::Linux {
                    desktop_file,
                    program,
                    args,
                },
            });
    }
    applications.into_values().collect()

}

#[cfg(target_os = "linux")]
/// 返回 Linux 桌面入口搜索目录。
fn linux_application_directories() -> Vec<PathBuf> {

    // 以下代码实现返回 Linux 桌面入口搜索目录。
    let mut directories = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        directories.push(PathBuf::from(data_home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join(".local/share/applications"));
    }

    let data_dirs =
        std::env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    directories
        .extend(std::env::split_paths(&data_dirs).map(|directory| directory.join("applications")));
    directories

}

#[cfg(target_os = "linux")]
/// 递归收集 Linux 桌面入口文件。
fn collect_linux_desktop_files(directory: &Path, depth: usize, desktop_files: &mut Vec<PathBuf>) {

    // 以下代码实现递归收集 Linux 桌面入口文件。
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == std::ffi::OsStr::new("desktop"))
        {
            desktop_files.push(path);
        } else if depth > 0 && path.is_dir() {
            collect_linux_desktop_files(&path, depth - 1, desktop_files);
        }
    }

}

#[cfg(target_os = "linux")]
/// 解析桌面入口中的编辑器信息。
fn parse_linux_text_editor_desktop_file(path: &Path) -> Option<(String, OsString, Vec<OsString>)> {

    // 以下代码实现解析桌面入口中的编辑器信息。
    let raw = fs::read_to_string(path).ok()?;
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut exec = None;
    let mut is_application = false;
    let mut supports_text = false;
    let mut hidden = false;
    let mut terminal = false;

    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Type" => is_application = value == "Application",
            "Name" => name = Some(value.to_string()),
            "Exec" => exec = Some(value.to_string()),
            "MimeType" => {
                supports_text = value.split(';').any(is_text_document_type);
            }
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }

    if !is_application || !supports_text || hidden || terminal {
        return None;
    }
    let name = name.filter(|value| !value.trim().is_empty())?;
    let (program, args) = parse_linux_desktop_exec(&exec?)?;
    Some((name, program, args))

}

#[cfg(target_os = "linux")]
/// 解析桌面入口的 Exec 命令。
fn parse_linux_desktop_exec(value: &str) -> Option<(OsString, Vec<OsString>)> {

    // 以下代码实现解析桌面入口的 Exec 命令。
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;

    for character in value.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character.is_whitespace() && !quoted {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped || quoted {
        return None;
    }
    if !current.is_empty() {
        parts.push(current);
    }

    let program = OsString::from(parts.first()?.as_str());
    let args = parts
        .into_iter()
        .skip(1)
        .filter_map(|part| {
            let part = ["%F", "%f", "%U", "%u", "%i", "%c", "%k"]
                .iter()
                .fold(part, |value, field_code| value.replace(*field_code, ""));
            (!part.is_empty()).then_some(OsString::from(part))
        })
        .collect();
    Some((program, args))

}

/// 保存文件管理器命令及其路径展示目标。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RevealCommandPlan {
    /// 操作系统命令程序。
    program: OsString,
    /// 命令需要的参数。
    args: Vec<OsString>,
    /// 用于错误信息展示的目标路径。
    display_target: PathBuf,
}

/// 表示文件打开命令使用的平台分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum RevealPlatform {
    Macos,
    Windows,
    Linux,
    Unsupported,
}

/// 执行当前平台的文件管理器定位操作。
fn reveal_path_impl(path: PathBuf) -> anyhow::Result<()> {

    // 以下代码实现执行当前平台的文件管理器定位操作。
    if !path.exists() {
        anyhow::bail!("path does not exist: {}", path.display());
    }

    let platform = reveal_platform();
    let (xdg_open, gio) = resolve_linux_openers(platform);

    let Some(plan) = build_reveal_command_plan(&path, platform, xdg_open, gio)? else {
        return Ok(());
    };

    let mut command = Command::new(&plan.program);
    command.args(&plan.args);
    spawn_path_command(command, &plan.display_target, "reveal")

}

/// 解析并打开路径所属目录。
fn open_containing_directory_impl(path: PathBuf) -> anyhow::Result<()> {

    // 以下代码实现解析并打开路径所属目录。
    let directory = resolve_containing_directory(&path)?;
    let directory = directory.canonicalize().with_context(|| {
        format!(
            "failed to resolve containing directory for: {}",
            path.display()
        )
    })?;
    reveal_path_impl(directory)

}

/// 计算文件或目录对应的所属目录。
fn resolve_containing_directory(path: &Path) -> anyhow::Result<PathBuf> {

    // 以下代码实现计算文件或目录对应的所属目录。
    if !path.exists() {
        anyhow::bail!("path does not exist: {}", path.display());
    }
    if path.is_dir() {
        return Ok(path.to_path_buf());
    }

    path.parent()
        .map(Path::to_path_buf)
        .context("file path does not have a parent directory")

}

/// 按配置应用打开路径并按原规则回退。
fn open_path_with_default_app_impl(path: PathBuf) -> anyhow::Result<()> {

    // 以下代码实现按配置应用打开路径并按原规则回退。
    if !path.exists() {
        anyhow::bail!("path does not exist: {}", path.display());
    }

    // let selected_editor_id = AppConfig::load_or_create()?
    let selected_editor_id = AppConfigService::load_or_create()?
        .general
        .default_file_open_target;
    if let Some(selected_editor_id) = selected_editor_id {
        if let Some(application) = detect_text_editor_applications()
            .iter()
            .find(|application| application.dto.id == selected_editor_id)
        {
            return launch_text_editor(application, &path);
        }
    }

    open_path_with_system_default_app_impl(&path)

}

/// 按编辑器标识启动并打开路径。
fn open_path_with_text_editor_impl(path: PathBuf, editor_id: Option<String>) -> anyhow::Result<()> {

    // 以下代码实现按编辑器标识启动并打开路径。
    if let Some(editor_id) = editor_id.filter(|value| !value.trim().is_empty()) {
        if !path.exists() {
            anyhow::bail!("path does not exist: {}", path.display());
        }

        let application = detect_text_editor_applications()
            .into_iter()
            .find(|application| application.dto.id == editor_id)
            .context("the selected text editor is no longer available")?;
        return launch_text_editor(&application, &path);
    }

    open_path_with_system_default_app_impl(&path)

}

/// 执行平台默认打开命令。
fn open_path_with_system_default_app_impl(path: &Path) -> anyhow::Result<()> {

    // 以下代码实现执行平台默认打开命令。
    if !path.exists() {
        anyhow::bail!("path does not exist: {}", path.display());
    }

    let platform = reveal_platform();
    let (xdg_open, gio) = resolve_linux_openers(platform);

    let Some(plan) = build_open_command_plan(path, platform, xdg_open, gio)? else {
        return Ok(());
    };

    let mut command = Command::new(&plan.program);
    command.args(&plan.args);
    spawn_path_command(command, &plan.display_target, "open")

}

/// 校验目标目录后复制源文件。
fn save_file_as_impl(source_path: PathBuf, destination_path: PathBuf) -> anyhow::Result<()> {

    // 以下代码实现校验目标目录后复制源文件。
    let source_metadata = fs::metadata(&source_path)
        .with_context(|| format!("failed to access source file: {}", source_path.display()))?;
    anyhow::ensure!(source_metadata.is_file(), "source path is not a file");

    let destination_parent = destination_path
        .parent()
        .context("destination path does not have a parent directory")?;
    anyhow::ensure!(
        destination_parent.is_dir(),
        "destination directory does not exist"
    );

    fs::copy(&source_path, &destination_path).with_context(|| {
        format!(
            "failed to save {} as {}",
            source_path.display(),
            destination_path.display()
        )
    })?;
    Ok(())

}

/// 读取文本文件并过滤二进制内容。
fn read_text_file_for_clipboard_impl(path: PathBuf) -> anyhow::Result<Option<String>> {

    // 以下代码实现读取文本文件并过滤二进制内容。
    let metadata = fs::metadata(&path)
        .with_context(|| format!("failed to access file: {}", path.display()))?;
    if !metadata.is_file() {
        return Ok(None);
    }

    let mut sample = [0_u8; 8 * 1024];
    let mut file = fs::File::open(&path)
        .with_context(|| format!("failed to open file: {}", path.display()))?;
    let sample_length = file
        .read(&mut sample)
        .with_context(|| format!("failed to inspect file: {}", path.display()))?;
    if sample[..sample_length].contains(&0) {
        return Ok(None);
    }

    let bytes =
        fs::read(&path).with_context(|| format!("failed to read file: {}", path.display()))?;
    if bytes.contains(&0) {
        return Ok(None);
    }

    Ok(String::from_utf8(bytes).ok())

}

/// 获取当前路径操作平台。
fn reveal_platform() -> RevealPlatform {

    // 以下代码实现获取当前路径操作平台。
    #[cfg(target_os = "macos")]
    {
        return RevealPlatform::Macos;
    }

    #[cfg(target_os = "windows")]
    {
        return RevealPlatform::Windows;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return RevealPlatform::Linux;
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        RevealPlatform::Unsupported
    }

}

/// 查找 Linux 路径打开程序。
fn resolve_linux_openers(platform: RevealPlatform) -> (Option<PathBuf>, Option<PathBuf>) {

    // 以下代码实现查找 Linux 路径打开程序。
    if platform == RevealPlatform::Linux {
        (
            crate::runtime_env::resolve_executable("xdg-open"),
            crate::runtime_env::resolve_executable("gio"),
        )
    } else {
        (None, None)
    }

}

/// 生成平台文件管理器定位命令。
fn build_reveal_command_plan(
    path: &Path,
    platform: RevealPlatform,
    xdg_open: Option<PathBuf>,
    gio: Option<PathBuf>,
) -> anyhow::Result<Option<RevealCommandPlan>> {

    // 以下代码实现生成平台文件管理器定位命令。
    let path_arg = path.as_os_str().to_os_string();

    match platform {
        RevealPlatform::Macos => {
            let mut args = Vec::with_capacity(2);
            if path.is_file() {
                args.push(OsString::from("-R"));
            }
            args.push(path_arg);
            Ok(Some(RevealCommandPlan {
                program: OsString::from("open"),
                args,
                display_target: path.to_path_buf(),
            }))
        }
        RevealPlatform::Windows => {
            let args = if path.is_file() {
                let mut select_arg = OsString::from("/select,");
                select_arg.push(path.as_os_str());
                vec![select_arg]
            } else {
                vec![path_arg]
            };

            Ok(Some(RevealCommandPlan {
                program: OsString::from("explorer.exe"),
                args,
                display_target: path.to_path_buf(),
            }))
        }
        RevealPlatform::Linux => {
            let target = if path.is_dir() {
                path.to_path_buf()
            } else {
                path.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf())
            };

            if let Some(program) = xdg_open {
                return Ok(Some(RevealCommandPlan {
                    program: program.into_os_string(),
                    args: vec![target.as_os_str().to_os_string()],
                    display_target: target,
                }));
            }

            if let Some(program) = gio {
                return Ok(Some(RevealCommandPlan {
                    program: program.into_os_string(),
                    args: vec![OsString::from("open"), target.as_os_str().to_os_string()],
                    display_target: target,
                }));
            }

            anyhow::bail!(
                "failed to reveal {}: neither xdg-open nor gio open is available",
                target.display()
            );
        }
        RevealPlatform::Unsupported => Ok(None),
    }

}

/// 生成平台默认打开命令。
fn build_open_command_plan(
    path: &Path,
    platform: RevealPlatform,
    xdg_open: Option<PathBuf>,
    gio: Option<PathBuf>,
) -> anyhow::Result<Option<RevealCommandPlan>> {

    // 以下代码实现生成平台默认打开命令。
    let path_arg = path.as_os_str().to_os_string();

    match platform {
        RevealPlatform::Macos => Ok(Some(RevealCommandPlan {
            program: OsString::from("open"),
            args: vec![path_arg],
            display_target: path.to_path_buf(),
        })),
        RevealPlatform::Windows => Ok(Some(RevealCommandPlan {
            program: OsString::from("cmd"),
            args: vec![
                OsString::from("/C"),
                OsString::from("start"),
                OsString::from(""),
                path_arg,
            ],
            display_target: path.to_path_buf(),
        })),
        RevealPlatform::Linux => {
            if let Some(program) = xdg_open {
                return Ok(Some(RevealCommandPlan {
                    program: program.into_os_string(),
                    args: vec![path.as_os_str().to_os_string()],
                    display_target: path.to_path_buf(),
                }));
            }

            if let Some(program) = gio {
                return Ok(Some(RevealCommandPlan {
                    program: program.into_os_string(),
                    args: vec![OsString::from("open"), path.as_os_str().to_os_string()],
                    display_target: path.to_path_buf(),
                }));
            }

            anyhow::bail!(
                "failed to open {}: neither xdg-open nor gio open is available",
                path.display()
            );
        }
        RevealPlatform::Unsupported => Ok(None),
    }

}

/// 启动路径操作系统命令。
fn spawn_path_command(
    mut command: Command,
    path: &std::path::Path,
    action: &str,
) -> anyhow::Result<()> {

    // 以下代码实现启动路径操作系统命令。
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("failed to {action} {}: {error}", path.display()))

}

/// 校验并解析仓库相对路径目标。
fn resolve_target_path_for_repo_lookup(
    access_root: &Path,
    file_path: &str,
) -> anyhow::Result<PathBuf> {

    // 以下代码实现校验并解析仓库相对路径目标。
    let target = access_root.join(fs_ops::validate_repo_relative_path(file_path)?);

    if target.exists() {
        let metadata = fs::symlink_metadata(&target).context("failed to resolve file path")?;
        let parent = target.parent().context("invalid file path")?;
        let parent_canonical = parent
            .canonicalize()
            .context("failed to resolve file path")?;
        anyhow::ensure!(
            parent_canonical.starts_with(access_root),
            "path traversal not allowed"
        );

        if metadata.file_type().is_symlink() {
            return Ok(target);
        }

        let canonical = target
            .canonicalize()
            .context("failed to resolve file path")?;
        anyhow::ensure!(
            canonical.starts_with(access_root),
            "path traversal not allowed"
        );
        return Ok(canonical);
    }

    let mut ancestor = target.parent().context("invalid file path")?;
    while !ancestor.exists() {
        ancestor = ancestor.parent().context("invalid file path")?;
    }

    let ancestor_canonical = ancestor
        .canonicalize()
        .context("ancestor directory not found")?;
    anyhow::ensure!(
        ancestor_canonical.starts_with(access_root),
        "path traversal not allowed"
    );

    let remainder = target
        .strip_prefix(ancestor)
        .context("failed to resolve target path")?;
    Ok(ancestor_canonical.join(remainder))

}

/// 解析工作区内编辑器文件引用。
fn resolve_editor_file_reference_impl(
    workspace_root: &str,
    raw_reference: &str,
) -> anyhow::Result<Option<ResolvedEditorFileReferenceDto>> {

    // 以下代码实现解析工作区内编辑器文件引用。
    let Some(parsed) = parse_editor_file_reference(raw_reference) else {
        return Ok(None);
    };

    let workspace_root = path_utils::canonicalize_path(Path::new(workspace_root))
        .context("failed to canonicalize workspace root")?;
    let candidate = if parsed.path.is_absolute() {
        parsed.path
    } else {
        workspace_root.join(parsed.path)
    };
    let Ok(resolved) = candidate.canonicalize() else {
        return Ok(None);
    };
    if !resolved.is_file() || !resolved.starts_with(&workspace_root) {
        return Ok(None);
    }
    let relative = resolved
        .strip_prefix(&workspace_root)
        .context("failed to resolve editor file path")?;
    Ok(Some(ResolvedEditorFileReferenceDto {
        root_path: workspace_root.to_string_lossy().to_string(),
        file_path: relative.to_string_lossy().to_string(),
        line: parsed.line,
        column: parsed.column,
    }))

}

/// 保存解析出的编辑器路径和光标位置。
#[derive(Debug)]
struct ParsedEditorFileReference {
    /// 文件引用中的目标路径。
    path: PathBuf,
    /// 可选的目标行号。
    line: Option<u32>,
    /// 可选的目标列号。
    column: Option<u32>,
}

/// 解析编辑器路径和位置。
fn parse_editor_file_reference(raw_reference: &str) -> Option<ParsedEditorFileReference> {

    // 以下代码实现解析编辑器路径和位置。
    let trimmed = raw_reference.trim();
    if trimmed.is_empty() || trimmed.contains("://") {
        return None;
    }

    let (path, line, column) = split_editor_reference_location(trimmed);
    let path = path.trim_start_matches("./");
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    if !Path::new(path).is_absolute() {
        fs_ops::validate_repo_relative_path(path).ok()?;
    }

    Some(ParsedEditorFileReference {
        path: PathBuf::from(path),
        line,
        column,
    })

}

/// 拆分文件引用中的行列信息。
fn split_editor_reference_location(value: &str) -> (&str, Option<u32>, Option<u32>) {

    // 以下代码实现拆分文件引用中的行列信息。
    if let Some((path, suffix)) = value.rsplit_once("#L") {
        if let Some((line, column)) = parse_line_column(suffix, '-') {
            return (path, line, column);
        }
    }

    let Some((path, line)) = value.rsplit_once(':') else {
        return (value, None, None);
    };
    let Some(line) = line.parse::<u32>().ok().filter(|line| *line > 0) else {
        return (value, None, None);
    };
    if let Some((path, column)) = path.rsplit_once(':') {
        if let Some(column) = column.parse::<u32>().ok().filter(|column| *column > 0) {
            return (path, Some(line), Some(column));
        }
    }
    (path, Some(line), None)

}

/// 解析引用中的行列数字。
fn parse_line_column(value: &str, separator: char) -> Option<(Option<u32>, Option<u32>)> {

    // 以下代码实现解析引用中的行列数字。
    let (line, column) = value
        .split_once(separator)
        .map_or((value, None), |(line, column)| (line, Some(column)));
    let line = line.parse::<u32>().ok().filter(|line| *line > 0)?;
    let column = column.and_then(|value| value.parse::<u32>().ok().filter(|column| *column > 0));
    Some((Some(line), column))

}

/// 经 WorkspaceService 解析工作区目标。
async fn load_workspace_target(
    state: &AppState,
    workspace_id: &str,
) -> Result<WorkspaceTarget, String> {
    // 以下代码通过 WorkspaceService 获取工作区及关联连接。
    WorkspaceService::new(state.clone())
        .resolve_workspace_target(workspace_id)
        .await
        .map_err(err_to_string)
}

/// 解析远程工作区文件根路径。
async fn remote_access_root(target: &WorkspaceTarget, root_path: &str) -> Result<String, String> {

    // 以下代码实现解析远程工作区文件根路径。
    let workspace_id = target.workspace.id.as_str();
    if root_path == remote_workspace_marker(workspace_id) || root_path == target.workspace.root_path
    {
        return Ok(target.workspace.root_path.clone());
    }
    if workspace_id_from_workspace_marker(root_path) == Some(workspace_id) {
        return Ok(target.workspace.root_path.clone());
    }
    Err("远端文件路径不属于当前项目".to_string())

}

/// 将业务错误转换成前端错误文本。
fn err_to_string(error: impl std::fmt::Display) -> String {

    // 以下代码实现将业务错误转换成前端错误文本。
    error.to_string()

}


#[cfg(test)]
#[path = "../../tests/unit/files_commands_tests.rs"]
mod tests;
