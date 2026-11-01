
    use super::{build_status, sdk_state};
    use crate::computer_control_sdk::{is_supported_platform, CuaDriverSdkStatus};

    fn sdk_status(initialized: bool, error: Option<&str>) -> CuaDriverSdkStatus {
        CuaDriverSdkStatus {
            initialized,
            resource_dir: None,
            library_path: None,
            abi_version: Some("1.1".to_string()),
            driver_version: None,
            embedded: Some(true),
            error: error.map(str::to_string),
        }
    }

    fn wayland_helper_status() -> crate::computer_control_sdk::CuaWaylandHelperStatus {
        crate::computer_control_sdk::CuaWaylandHelperStatus {
            supported: cfg!(all(target_os = "linux", target_arch = "x86_64")),
            wayland: false,
            installed: false,
            running: false,
        }
    }

    #[test]
    fn enabled_but_uninitialized_sdk_is_not_reported_as_ready() {
        let status = sdk_status(false, None);
        let result = build_status(true, status.clone(), wayland_helper_status(), Vec::new());

        if is_supported_platform() {
            assert_eq!(sdk_state(false, &status), "disabled");
            assert_eq!(sdk_state(true, &status), "uninitialized");
            assert_eq!(result.sdk.state, "uninitialized");
        } else {
            assert_eq!(sdk_state(false, &status), "unsupported");
            assert_eq!(sdk_state(true, &status), "unsupported");
            assert_eq!(result.sdk.state, "unsupported");
        }
        assert!(!result.sdk.initialized);
        assert_eq!(result.adapters.len(), 3);
        assert!(result.adapters.iter().all(|adapter| adapter.built_in));
    }

    #[test]
    fn initialized_sdk_is_ready_and_failed_sdk_is_reported() {
        if is_supported_platform() {
            assert_eq!(sdk_state(true, &sdk_status(true, None)), "ready");
            assert_eq!(
                sdk_state(true, &sdk_status(false, Some("load failed"))),
                "failed"
            );
        }
    }
