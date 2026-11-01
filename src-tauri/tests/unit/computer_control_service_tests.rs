
    use super::{
        application_resource, dynamic_tool_failure, dynamic_tool_success, request_pid,
        request_targets_current_process, resolved_application_resource, target_resource,
        ComputerControlService,
    };
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn dynamic_tools_are_not_registered_before_sdk_is_ready() {
        let service = ComputerControlService::default();
        assert!(!service.sdk().status().initialized);
        assert_eq!(
            service
                .dynamic_tools_spec()
                .expect("an uninitialized SDK should register no tools"),
            json!([])
        );
        let _ = Arc::new(service);
    }

    /*
    #[test]
    fn desktop_scope_and_unscoped_input_are_rejected() {
        assert!(target_resource("click", &json!({"scope": "desktop"})).is_err());
        assert!(target_resource("click", &json!({"x": 10, "y": 20})).is_err());
        assert!(target_resource("click", &json!({"path": "notepad.exe"})).is_ok());
    }
    */

    #[test]
    fn desktop_scope_is_rejected_and_unscoped_input_is_accepted() {
        assert!(target_resource("click", &json!({"scope": "desktop"})).is_err());
        assert!(target_resource("click", &json!({"x": 10, "y": 20})).is_ok());
        let current_exe_path = std::env::current_exe()
            .expect("the current executable path should resolve")
            .to_string_lossy()
            .to_string();
        assert!(target_resource("click", &json!({"path": current_exe_path})).is_ok());
    }

    #[test]
    fn accessibility_tree_without_target_uses_derived_tool_resource() {
        let target = target_resource("get_accessibility_tree", &json!({}))
            .expect("the SDK tool contract allows an empty argument object");

        assert_eq!(target.key, "tool:get_accessibility_tree");
        assert_eq!(target.scope, "observe");
    }

    #[test]
    fn future_sdk_tool_without_target_uses_input_resource() {
        let target = target_resource("future_sdk_observer", &json!({}))
            .expect("future SDK tools should receive a derived authorization resource");

        assert_eq!(target.key, "tool:future_sdk_observer");
        assert_eq!(target.scope, "input");
    }

    #[test]
    fn current_pid_is_the_only_self_process_match() {
        let current_pid = std::process::id();
        let arguments = json!({"pid": current_pid});

        assert_eq!(request_pid(&arguments), Some(current_pid));
        assert!(request_targets_current_process(&arguments));
    }

    #[test]
    fn different_pid_is_not_treated_as_self_process() {
        let current_pid = std::process::id();
        let different_pid = if current_pid == u32::MAX {
            u32::MIN
        } else {
            current_pid.wrapping_add(1)
        };
        let arguments = json!({"pid": different_pid});

        assert_eq!(request_pid(&arguments), Some(different_pid));
        assert!(!request_targets_current_process(&arguments));
    }

    #[test]
    fn same_name_without_pid_is_not_treated_as_self_process() {
        let arguments = json!({"application": "AuraCoder"});

        assert_eq!(request_pid(&arguments), None);
        assert!(!request_targets_current_process(&arguments));
    }

    #[test]
    fn application_file_name_is_extracted_without_guessed_extension() {
        let system_notepad = application_resource(r"C:\\Windows\\System32\\notepad.exe");
        let custom_notepad = application_resource(r"D:\\tools\\notepad.exe");
        let custom_extension = application_resource(r"D:\\tools\\notepad.custom");
        let bare_notepad = application_resource("Notepad");

        assert_eq!(system_notepad.key, "application:notepad.exe");
        assert_eq!(custom_notepad.key, "application:notepad.exe");
        assert_eq!(custom_extension.key, "application:notepad.custom");
        assert_eq!(bare_notepad.key, "application:notepad");
        assert_eq!(system_notepad.display, "notepad.exe");
        assert_eq!(custom_notepad.display, "notepad.exe");
        assert_eq!(custom_extension.display, "notepad.custom");
        assert_eq!(bare_notepad.display, "Notepad");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn bare_windows_name_uses_windows_actual_executable_name() {
        let notepad = resolved_application_resource("notepad")
            .expect("Windows should resolve the built-in Notepad executable");
        let notepad_exe = resolved_application_resource("notepad.exe")
            .expect("Windows should resolve the built-in Notepad executable");

        assert_eq!(notepad.key, "application:notepad.exe");
        assert_eq!(notepad.display.to_ascii_lowercase(), "notepad.exe");
        assert_eq!(notepad.key, notepad_exe.key);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_pid_uses_proc_executable_name() {
        let target = target_resource("list_windows", &json!({"pid": std::process::id()}))
            .expect("Linux should resolve the current process through /proc");
        let current_name = std::env::current_exe()
            .expect("current executable path should resolve")
            .file_name()
            .expect("current executable should have a file name")
            .to_string_lossy()
            .to_lowercase();

        assert_eq!(target.key, format!("application:{current_name}"));
        assert_eq!(target.scope, "application");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_pid_uses_proc_executable_name() {
        let target = target_resource("list_windows", &json!({"pid": std::process::id()}))
            .expect("macOS should resolve the current process through proc_pidpath");
        let current_name = std::env::current_exe()
            .expect("current executable path should resolve")
            .file_name()
            .expect("current executable should have a file name")
            .to_string_lossy()
            .to_lowercase();

        assert_eq!(target.key, format!("application:{current_name}"));
        assert_eq!(target.scope, "application");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_application_name_and_bundle_id_share_the_bundle_authorization() {
        let by_name = target_resource("launch_app", &json!({"name": "Calculator"}))
            .expect("macOS application name should resolve through system metadata");
        let by_bundle_id =
            target_resource("launch_app", &json!({"bundle_id": "com.apple.calculator"}))
                .expect("macOS bundle identifier should resolve through system metadata");

        assert_eq!(by_name.key, "application:calculator.app");
        assert_eq!(by_name.key, by_bundle_id.key);
        assert_eq!(by_name.display, by_bundle_id.display);
        assert_eq!(by_name.scope, by_bundle_id.scope);
    }

    #[test]
    fn unresolved_bare_name_is_not_given_an_extension() {
        assert!(resolved_application_resource("auracoder-cua-not-a-real-application").is_err());
    }

    #[test]
    fn target_scopes_are_classified() {
        assert_eq!(
            target_resource("start_session", &json!({}))
                .expect("session target should resolve")
                .scope,
            "metadata"
        );
        assert_eq!(
            target_resource("list_windows", &json!({}))
                .expect("observation target should resolve")
                .scope,
            "observation"
        );
        assert_eq!(
            target_resource("launch_app", &json!({"path": "notepad.exe"}))
                .expect("application target should resolve")
                .scope,
            "application"
        );
    }

    #[test]
    fn dynamic_tool_result_has_codex_content_items() {
        assert_eq!(dynamic_tool_success(json!({"ok": true}))["success"], true);
        assert_eq!(
            dynamic_tool_failure("permission_denied: no")["success"],
            false
        );
        let image = dynamic_tool_success(json!({
            "content": [{"type": "image", "data": "AQ==", "mimeType": "image/png"}]
        }));
        assert_eq!(image["contentItems"][0]["type"], "inputImage");
    }
