
    use std::{fs, path::PathBuf};

    #[cfg(target_os = "windows")]
    use super::extract_windows_launch_program;
    use super::{
        build_open_command_plan, build_reveal_command_plan, is_text_document_type,
        read_text_file_for_clipboard_impl, resolve_containing_directory,
        resolve_target_path_for_repo_lookup, save_file_as_impl, RevealPlatform,
    };
    use uuid::Uuid;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    /// 为文件业务测试创建临时目录和文本文件。
    fn with_temp_path<T>(f: impl FnOnce(PathBuf, PathBuf) -> T) -> T {
        let root = std::env::temp_dir().join(format!("auracoder-reveal-path-{}", Uuid::new_v4()));
        let dir = root.join("nested");
        let file = dir.join("file.txt");
        fs::create_dir_all(&dir).expect("temp dir should exist");
        fs::write(&file, "hello").expect("temp file should exist");
        let result = f(dir.clone(), file.clone());
        let _ = fs::remove_dir_all(&root);
        result
    }

    /// 验证文本文件类型识别规则。
    #[test]
    fn identifies_registered_plain_text_document_types() {
        assert!(is_text_document_type(".txt"));
        assert!(is_text_document_type("text/plain"));
        assert!(is_text_document_type("text/markdown"));
        assert!(is_text_document_type("public.source-code"));
        assert!(!is_text_document_type("image/png"));
        assert!(!is_text_document_type("application/pdf"));
    }

    /// 验证剪贴板文件读取返回文本并忽略二进制文件。
    #[test]
    fn clipboard_reader_returns_text_and_ignores_binary_files() {
        with_temp_path(|_dir, file| {
            assert_eq!(
                read_text_file_for_clipboard_impl(file.clone()).expect("text should be readable"),
                Some("hello".to_string())
            );

            fs::write(&file, [0x4d, 0x5a, 0, 0x90]).expect("binary file should be writable");
            assert_eq!(
                read_text_file_for_clipboard_impl(file).expect("binary inspection should succeed"),
                None
            );
        });
    }

    /// 验证另存为操作复制源文件。
    #[test]
    fn save_file_as_copies_the_source_file() {
        with_temp_path(|dir, file| {
            let destination = dir.join("copied-file.txt");
            save_file_as_impl(file, destination.clone()).expect("file should be copied");
            assert_eq!(
                fs::read_to_string(destination).expect("copy should be readable"),
                "hello"
            );
        });
    }

    /// 验证文件与目录的所属目录解析。
    #[test]
    fn resolves_the_containing_directory_for_a_file() {
        with_temp_path(|dir, file| {
            assert_eq!(
                resolve_containing_directory(&file).expect("file should have a parent"),
                dir.clone()
            );
            assert_eq!(
                resolve_containing_directory(&dir).expect("directory should resolve to itself"),
                dir
            );
        });
    }

    /// 验证 Windows 注册表命令中的程序路径解析。
    #[cfg(target_os = "windows")]
    #[test]
    fn extracts_the_executable_from_a_registered_windows_open_command() {
        let program = extract_windows_launch_program(
            r#""C:\\Program Files\\Example Editor\\editor.exe" --reuse-window "%1""#,
        )
        .expect("registered command should contain an executable");

        assert_eq!(
            program,
            PathBuf::from(r#"C:\\Program Files\\Example Editor\\editor.exe"#)
        );
    }

    /// 验证 Windows 文件定位命令使用 Explorer 选择参数。
    #[test]
    fn windows_files_use_explorer_select_args() {
        with_temp_path(|_dir, file| {
            let plan = build_reveal_command_plan(&file, RevealPlatform::Windows, None, None)
                .expect("plan should build")
                .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "explorer.exe");
            assert_eq!(plan.args.len(), 1);
            assert_eq!(
                plan.args[0].to_string_lossy(),
                format!("/select,{}", file.display())
            );
            assert_eq!(plan.display_target, file);
        });
    }

    /// 验证 Windows 目录定位命令使用 Explorer。
    #[test]
    fn windows_directories_open_in_explorer() {
        with_temp_path(|dir, _file| {
            let plan = build_reveal_command_plan(&dir, RevealPlatform::Windows, None, None)
                .expect("plan should build")
                .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "explorer.exe");
            assert_eq!(plan.args, vec![dir.as_os_str().to_os_string()]);
            assert_eq!(plan.display_target, dir);
        });
    }

    /// 验证 macOS 文件定位命令使用 open 的定位标记。
    #[test]
    fn mac_files_use_open_reveal_flag() {
        with_temp_path(|_dir, file| {
            let plan = build_reveal_command_plan(&file, RevealPlatform::Macos, None, None)
                .expect("plan should build")
                .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "open");
            assert_eq!(
                plan.args,
                vec![
                    std::ffi::OsString::from("-R"),
                    file.as_os_str().to_os_string()
                ]
            );
        });
    }

    /// 验证 Linux 文件定位优先打开父目录。
    #[test]
    fn linux_prefers_xdg_open_for_parent_directory() {
        with_temp_path(|dir, file| {
            let plan = build_reveal_command_plan(
                &file,
                RevealPlatform::Linux,
                Some(PathBuf::from("/usr/bin/xdg-open")),
                Some(PathBuf::from("/usr/bin/gio")),
            )
            .expect("plan should build")
            .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "/usr/bin/xdg-open");
            assert_eq!(plan.args, vec![dir.as_os_str().to_os_string()]);
            assert_eq!(plan.display_target, dir);
        });
    }

    /// 验证缺少 xdg-open 时使用 gio 定位目录。
    #[test]
    fn linux_falls_back_to_gio_when_xdg_open_is_missing() {
        with_temp_path(|dir, _file| {
            let plan = build_reveal_command_plan(
                &dir,
                RevealPlatform::Linux,
                None,
                Some(PathBuf::from("/usr/bin/gio")),
            )
            .expect("plan should build")
            .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "/usr/bin/gio");
            assert_eq!(
                plan.args,
                vec![
                    std::ffi::OsString::from("open"),
                    dir.as_os_str().to_os_string()
                ]
            );
            assert_eq!(plan.display_target, dir);
        });
    }

    /// 验证 Linux 缺少路径打开程序时返回明确错误。
    #[test]
    fn linux_returns_a_clear_error_without_openers() {
        with_temp_path(|dir, _file| {
            let error = build_reveal_command_plan(&dir, RevealPlatform::Linux, None, None)
                .expect_err("missing openers should fail");

            assert!(error
                .to_string()
                .contains("neither xdg-open nor gio open is available"));
        });
    }

    /// 验证 macOS 默认打开命令使用 open。
    #[test]
    fn mac_files_use_open_for_default_app() {
        with_temp_path(|_dir, file| {
            let plan = build_open_command_plan(&file, RevealPlatform::Macos, None, None)
                .expect("plan should build")
                .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "open");
            assert_eq!(plan.args, vec![file.as_os_str().to_os_string()]);
            assert_eq!(plan.display_target, file);
        });
    }

    /// 验证 Windows 默认打开命令使用 cmd start。
    #[test]
    fn windows_files_use_cmd_start_for_default_app() {
        with_temp_path(|_dir, file| {
            let plan = build_open_command_plan(&file, RevealPlatform::Windows, None, None)
                .expect("plan should build")
                .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "cmd");
            assert_eq!(
                plan.args,
                vec![
                    std::ffi::OsString::from("/C"),
                    std::ffi::OsString::from("start"),
                    std::ffi::OsString::from(""),
                    file.as_os_str().to_os_string(),
                ]
            );
            assert_eq!(plan.display_target, file);
        });
    }

    /// 验证 Linux 默认打开优先使用 xdg-open。
    #[test]
    fn linux_prefers_xdg_open_for_default_app() {
        with_temp_path(|_dir, file| {
            let plan = build_open_command_plan(
                &file,
                RevealPlatform::Linux,
                Some(PathBuf::from("/usr/bin/xdg-open")),
                Some(PathBuf::from("/usr/bin/gio")),
            )
            .expect("plan should build")
            .expect("plan should exist");

            assert_eq!(plan.program.to_string_lossy(), "/usr/bin/xdg-open");
            assert_eq!(plan.args, vec![file.as_os_str().to_os_string()]);
            assert_eq!(plan.display_target, file);
        });
    }

    /// 验证仓库路径允许位于尚未创建的嵌套目录中。
    #[test]
    fn resolve_target_path_for_repo_lookup_allows_nested_missing_parents() {
        with_temp_path(|dir, _file| {
            let root = dir.parent().expect("root should exist").to_path_buf();
            let canonical_root = root.canonicalize().expect("root should resolve");

            let resolved = resolve_target_path_for_repo_lookup(
                &canonical_root,
                "src/components/FileExplorer.tsx",
            )
            .expect("nested path should resolve");

            assert_eq!(
                resolved,
                canonical_root.join("src/components/FileExplorer.tsx")
            );
        });
    }

    /// 验证仓库路径拒绝越出根目录的父路径。
    #[test]
    fn resolve_target_path_for_repo_lookup_rejects_missing_parent_traversal() {
        with_temp_path(|dir, _file| {
            let root = dir.parent().expect("root should exist").to_path_buf();
            let canonical_root = root.canonicalize().expect("root should resolve");

            let error =
                resolve_target_path_for_repo_lookup(&canonical_root, "../outside/FileExplorer.tsx")
                    .expect_err("path traversal should fail");

            assert!(error.to_string().contains("invalid file or directory path"));
        });
    }

    /// 验证缺失祖先路径中的父目录分量会被拒绝。
    #[test]
    fn resolve_target_path_for_repo_lookup_rejects_parent_dir_components_inside_missing_ancestors()
    {
        with_temp_path(|dir, _file| {
            let root = dir.parent().expect("root should exist").to_path_buf();
            let canonical_root = root.canonicalize().expect("root should resolve");

            let error =
                resolve_target_path_for_repo_lookup(&canonical_root, "missing/../nested/file.txt")
                    .expect_err("unresolved parent segments should be rejected");

            assert!(error.to_string().contains("invalid file or directory path"));
        });
    }

    /// 验证仓库路径保留符号链接目录项。
    #[cfg(unix)]
    #[test]
    fn resolve_target_path_for_repo_lookup_preserves_symlink_entries() {
        with_temp_path(|dir, _file| {
            let root = dir.parent().expect("root should exist").to_path_buf();
            let canonical_root = root.canonicalize().expect("root should resolve");
            symlink("nested/file.txt", root.join("link.txt")).expect("symlink should exist");

            let resolved = resolve_target_path_for_repo_lookup(&canonical_root, "link.txt")
                .expect("symlink path should resolve");

            assert_eq!(resolved, canonical_root.join("link.txt"));
        });
    }
