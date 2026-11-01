
    use super::*;
    use std::{collections::HashMap, fs, sync::Mutex as StdMutex};
    use uuid::Uuid;

    struct FakeSpawner {
        support: SupportStatus,
        next_spawn: StdMutex<Vec<anyhow::Result<SpawnedKeepAwakeChild>>>,
    }

    #[derive(Debug, Default)]
    struct FakeProcessOps {
        commands: StdMutex<HashMap<u32, Option<String>>>,
        start_markers: StdMutex<HashMap<u32, Option<String>>>,
        terminated: StdMutex<Vec<u32>>,
        terminate_error: StdMutex<Option<String>>,
    }

    #[derive(Debug)]
    struct FakeChildState {
        alive: bool,
        kill_error: Option<String>,
        wait_error: Option<String>,
        exit_code: i32,
    }

    #[derive(Debug, Clone)]
    struct FakeChildHandle {
        state: Arc<StdMutex<FakeChildState>>,
    }

    impl FakeChildHandle {
        fn new(exit_code: i32) -> (Self, Arc<StdMutex<FakeChildState>>) {
            let state = Arc::new(StdMutex::new(FakeChildState {
                alive: true,
                kill_error: None,
                wait_error: None,
                exit_code,
            }));
            (
                Self {
                    state: state.clone(),
                },
                state,
            )
        }
    }

    #[async_trait]
    impl KeepAwakeChild for FakeChildHandle {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            let state = self.state.lock().expect("fake child state lock poisoned");
            if state.alive {
                Ok(None)
            } else {
                Ok(Some(exit_status_from_code(state.exit_code)))
            }
        }

        async fn kill(&mut self) -> io::Result<()> {
            let mut state = self.state.lock().expect("fake child state lock poisoned");
            if let Some(error) = &state.kill_error {
                return Err(io::Error::other(error.clone()));
            }
            state.alive = false;
            Ok(())
        }

        async fn wait(&mut self) -> io::Result<ExitStatus> {
            let mut state = self.state.lock().expect("fake child state lock poisoned");
            if let Some(error) = &state.wait_error {
                return Err(io::Error::other(error.clone()));
            }
            state.alive = false;
            Ok(exit_status_from_code(state.exit_code))
        }
    }

    impl KeepAwakeSpawner for FakeSpawner {
        fn support_status(&self) -> SupportStatus {
            self.support.clone()
        }

        fn spawn(&self, _profile: &PowerProfile) -> anyhow::Result<SpawnedKeepAwakeChild> {
            match self
                .next_spawn
                .lock()
                .expect("fake spawner lock poisoned")
                .pop()
            {
                Some(next) => next,
                None => anyhow::bail!("no fake child configured"),
            }
        }
    }

    impl KeepAwakeProcessOps for FakeProcessOps {
        fn read_command_line(&self, pid: u32) -> io::Result<Option<String>> {
            Ok(self
                .commands
                .lock()
                .expect("fake commands lock poisoned")
                .get(&pid)
                .cloned()
                .flatten())
        }

        fn terminate(&self, pid: u32) -> io::Result<()> {
            if let Some(error) = self
                .terminate_error
                .lock()
                .expect("fake terminate error lock poisoned")
                .clone()
            {
                return Err(io::Error::other(error));
            }

            self.terminated
                .lock()
                .expect("fake terminated lock poisoned")
                .push(pid);
            Ok(())
        }

        fn read_start_marker(&self, pid: u32) -> io::Result<Option<String>> {
            Ok(self
                .start_markers
                .lock()
                .expect("fake start markers lock poisoned")
                .get(&pid)
                .cloned()
                .flatten())
        }
    }

    fn make_spawn(child: FakeChildHandle, pid: u32) -> SpawnedKeepAwakeChild {
        SpawnedKeepAwakeChild {
            child: Box::new(child),
            helper: Some(KeepAwakeHelperState {
                pid,
                program: "/usr/bin/caffeinate".to_string(),
                args: vec!["-i".to_string(), "-w".to_string(), "1".to_string()],
                start_marker: Some(format!("start-{pid}")),
            }),
        }
    }

    fn test_process(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_marker: Some(format!("owner-{pid}")),
        }
    }

    fn temp_state_dir() -> PathBuf {
        std::env::temp_dir().join(format!("auracoder-keep-awake-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn reports_unsupported_runtime() {
        let current_process = test_process(1);
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: false,
                    message: Some("unsupported".to_string()),
                },
                next_spawn: StdMutex::new(Vec::new()),
            }),
            Arc::new(FakeProcessOps::default()),
            temp_state_dir(),
            current_process,
        );

        let status = manager.status().await;
        assert!(!status.supported);
        assert!(!status.active);
        assert_eq!(status.message.as_deref(), Some("unsupported"));
        assert!(manager.enable().await.is_err());
    }

    #[tokio::test]
    async fn enable_and_disable_are_idempotent() {
        let (child, _state) = FakeChildHandle::new(0);
        let current_process = test_process(2);
        let state_dir = temp_state_dir();
        let state_path = state_file_path(&state_dir, current_process.pid);
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(vec![Ok(make_spawn(child, 101))]),
            }),
            Arc::new(FakeProcessOps::default()),
            state_dir,
            current_process,
        );

        manager.enable().await.expect("enable should succeed");
        assert!(state_path.exists());
        manager
            .enable()
            .await
            .expect("second enable should be a no-op");
        assert!(manager.status().await.active);

        manager.disable().await.expect("disable should succeed");
        manager
            .disable()
            .await
            .expect("second disable should be a no-op");
        assert!(!manager.status().await.active);
        assert_eq!(manager.status().await.message, None);
        assert!(!state_path.exists());
    }

    #[tokio::test]
    async fn enable_fails_when_helper_exits_immediately() {
        let (child, state) = FakeChildHandle::new(23);
        state.lock().expect("fake child state lock poisoned").alive = false;
        let current_process = test_process(22);
        let state_dir = temp_state_dir();
        let state_path = state_file_path(&state_dir, current_process.pid);
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(vec![Ok(make_spawn(child, 222))]),
            }),
            Arc::new(FakeProcessOps::default()),
            state_dir,
            current_process,
        );

        let error = manager
            .enable()
            .await
            .expect_err("enable should fail when helper exits immediately");
        assert!(error.contains("status code 23"));

        let status = manager.status().await;
        assert!(!status.active);
        assert_eq!(
            status.message.as_deref(),
            Some("keep awake helper exited unexpectedly with status code 23")
        );
        assert!(!state_path.exists());
    }

    #[tokio::test]
    async fn status_reflects_unexpected_child_exit() {
        let (child, state) = FakeChildHandle::new(17);
        let current_process = test_process(3);
        let state_dir = temp_state_dir();
        let state_path = state_file_path(&state_dir, current_process.pid);
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(vec![Ok(make_spawn(child, 202))]),
            }),
            Arc::new(FakeProcessOps::default()),
            state_dir,
            current_process,
        );

        manager.enable().await.expect("enable should succeed");
        state.lock().expect("fake child state lock poisoned").alive = false;

        let status = manager.status().await;
        assert!(!status.active);
        assert_eq!(
            status.message.as_deref(),
            Some("keep awake helper exited unexpectedly with status code 17")
        );
        assert!(!state_path.exists());
    }

    #[tokio::test]
    async fn disable_failure_keeps_helper_tracked() {
        let (child, state) = FakeChildHandle::new(0);
        state
            .lock()
            .expect("fake child state lock poisoned")
            .kill_error = Some("permission denied".to_string());
        let current_process = test_process(4);
        let state_dir = temp_state_dir();
        let state_path = state_file_path(&state_dir, current_process.pid);
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(vec![Ok(make_spawn(child, 303))]),
            }),
            Arc::new(FakeProcessOps::default()),
            state_dir,
            current_process,
        );

        manager.enable().await.expect("enable should succeed");
        let error = manager
            .disable()
            .await
            .expect_err("disable should surface kill failures");
        assert!(error.contains("failed to stop keep awake helper"));
        assert!(manager.status().await.active);
        assert!(state_path.exists());
    }

    #[tokio::test]
    async fn ac_resume_failure_preserves_paused_state_and_reports_error() {
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(vec![Err(anyhow::anyhow!("resume failed"))]),
            }),
            Arc::new(FakeProcessOps::default()),
            temp_state_dir(),
            test_process(40),
        );

        {
            let mut runtime = manager.runtime.lock().await;
            runtime.active_profile = Some(PowerProfile {
                ac_only: true,
                ..PowerProfile::default_profile()
            });
            runtime.paused_due_to_battery = true;
            runtime.on_ac_power = Some(false);
        }

        let (event_tx, event_rx) = tokio::sync::mpsc::channel(2);
        event_tx
            .send(MonitorEvent::AcStatusChanged { on_ac: true })
            .await
            .expect("event should send");
        drop(event_tx);

        manager.process_monitor_events(event_rx).await;

        let status = manager.status().await;
        assert!(!status.active);
        assert_eq!(status.on_ac_power, Some(true));
        assert!(!status.paused_due_to_battery);
        assert_eq!(
            status.message.as_deref(),
            Some("failed to resume keep awake on AC power: resume failed")
        );
    }

    #[test]
    fn reclaim_stale_helpers_skip_live_owner_processes() {
        let state_dir = temp_state_dir();
        let state_path = state_file_path(&state_dir, 10);
        save_helper_state(
            &state_path,
            &PersistedKeepAwakeHelper {
                owner: test_process(10),
                helper: KeepAwakeHelperState {
                    pid: 404,
                    program: "/usr/bin/caffeinate".to_string(),
                    args: vec!["-i".to_string(), "-w".to_string(), "1".to_string()],
                    start_marker: Some("start-404".to_string()),
                },
            },
        )
        .expect("helper state should save");

        let process_ops = Arc::new(FakeProcessOps::default());
        process_ops
            .commands
            .lock()
            .expect("fake commands lock poisoned")
            .insert(
                10,
                Some("/Applications/AuraCoder.app/Contents/MacOS/AuraCoder".to_string()),
            );
        process_ops
            .start_markers
            .lock()
            .expect("fake start markers lock poisoned")
            .insert(10, Some("owner-10".to_string()));
        process_ops
            .commands
            .lock()
            .expect("fake commands lock poisoned")
            .insert(404, Some("/usr/bin/caffeinate -i -w 1".to_string()));
        process_ops
            .start_markers
            .lock()
            .expect("fake start markers lock poisoned")
            .insert(404, Some("start-404".to_string()));
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(Vec::new()),
            }),
            process_ops.clone(),
            state_dir,
            test_process(99),
        );

        manager
            .reclaim_stale_helpers()
            .expect("live helper should not be reclaimed");

        assert!(process_ops
            .terminated
            .lock()
            .expect("fake terminated lock poisoned")
            .is_empty());
        assert!(state_path.exists());
    }

    #[test]
    fn reclaim_stale_helpers_terminates_matching_process() {
        let state_dir = temp_state_dir();
        let state_path = state_file_path(&state_dir, 11);
        save_helper_state(
            &state_path,
            &PersistedKeepAwakeHelper {
                owner: test_process(11),
                helper: KeepAwakeHelperState {
                    pid: 405,
                    program: "/usr/bin/caffeinate".to_string(),
                    args: vec!["-i".to_string(), "-w".to_string(), "1".to_string()],
                    start_marker: Some("start-405".to_string()),
                },
            },
        )
        .expect("helper state should save");

        let process_ops = Arc::new(FakeProcessOps::default());
        process_ops
            .commands
            .lock()
            .expect("fake commands lock poisoned")
            .insert(405, Some("/usr/bin/caffeinate -i -w 1".to_string()));
        process_ops
            .start_markers
            .lock()
            .expect("fake start markers lock poisoned")
            .insert(405, Some("start-405".to_string()));
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(Vec::new()),
            }),
            process_ops.clone(),
            state_dir,
            test_process(99),
        );

        manager
            .reclaim_stale_helpers()
            .expect("stale helper reclaim should succeed");

        assert_eq!(
            process_ops
                .terminated
                .lock()
                .expect("fake terminated lock poisoned")
                .as_slice(),
            &[405]
        );
        assert!(!state_path.exists());
    }

    #[test]
    fn reclaim_stale_helpers_terminate_when_saved_start_marker_is_missing() {
        let state_dir = temp_state_dir();
        let state_path = state_file_path(&state_dir, 12);
        save_helper_state(
            &state_path,
            &PersistedKeepAwakeHelper {
                owner: test_process(12),
                helper: KeepAwakeHelperState {
                    pid: 406,
                    program: "/usr/bin/caffeinate".to_string(),
                    args: vec!["-i".to_string(), "-w".to_string(), "1".to_string()],
                    start_marker: None,
                },
            },
        )
        .expect("helper state should save");

        let process_ops = Arc::new(FakeProcessOps::default());
        process_ops
            .commands
            .lock()
            .expect("fake commands lock poisoned")
            .insert(406, Some("/usr/bin/caffeinate -i -w 1".to_string()));
        process_ops
            .start_markers
            .lock()
            .expect("fake start markers lock poisoned")
            .insert(406, Some("start-406".to_string()));
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(Vec::new()),
            }),
            process_ops.clone(),
            state_dir,
            test_process(99),
        );

        manager
            .reclaim_stale_helpers()
            .expect("stale helper reclaim should succeed");

        assert_eq!(
            process_ops
                .terminated
                .lock()
                .expect("fake terminated lock poisoned")
                .as_slice(),
            &[406]
        );
        assert!(!state_path.exists());
    }

    #[test]
    fn reclaim_stale_helpers_skips_invalid_state_files() {
        let state_dir = temp_state_dir();
        let invalid_state_path = state_file_path(&state_dir, 12);
        fs::create_dir_all(&state_dir).expect("state dir should exist");
        fs::write(&invalid_state_path, b"{not-json").expect("invalid helper state should save");

        let valid_state_path = state_file_path(&state_dir, 13);
        save_helper_state(
            &valid_state_path,
            &PersistedKeepAwakeHelper {
                owner: test_process(13),
                helper: KeepAwakeHelperState {
                    pid: 407,
                    program: "/usr/bin/caffeinate".to_string(),
                    args: vec!["-i".to_string(), "-w".to_string(), "1".to_string()],
                    start_marker: Some("start-407".to_string()),
                },
            },
        )
        .expect("valid helper state should save");

        let process_ops = Arc::new(FakeProcessOps::default());
        process_ops
            .commands
            .lock()
            .expect("fake commands lock poisoned")
            .insert(407, Some("/usr/bin/caffeinate -i -w 1".to_string()));
        process_ops
            .start_markers
            .lock()
            .expect("fake start markers lock poisoned")
            .insert(407, Some("start-407".to_string()));
        let manager = KeepAwakeManager::with_dependencies(
            Arc::new(FakeSpawner {
                support: SupportStatus {
                    supported: true,
                    message: None,
                },
                next_spawn: StdMutex::new(Vec::new()),
            }),
            process_ops.clone(),
            state_dir,
            test_process(99),
        );

        manager
            .reclaim_stale_helpers()
            .expect("reclaim should continue past invalid state");

        assert_eq!(
            process_ops
                .terminated
                .lock()
                .expect("fake terminated lock poisoned")
                .as_slice(),
            &[407]
        );
        assert!(!invalid_state_path.exists());
        assert!(!valid_state_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_backend_blocks_sleep_and_idle_for_current_process() {
        let profile = PowerProfile::default_profile();
        let spec = resolve_backend_spec(&profile).expect("linux backend should resolve");
        let program = spec.program.to_string_lossy().into_owned();
        let args = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        if program.contains("systemd-inhibit") {
            assert!(args
                .iter()
                .any(|arg| arg == "--what=idle:sleep:handle-lid-switch"));
        } else {
            assert!(program.contains("gnome-session-inhibit"));
            assert!(args.windows(2).any(|pair| pair == ["--inhibit", "suspend"]));
            assert!(args.windows(2).any(|pair| pair == ["--inhibit", "idle"]));
        }
        assert!(args.iter().any(|arg| arg == "-f"));
        assert!(args.iter().any(|arg| arg == "/dev/null"));
        assert!(args
            .iter()
            // 旧写法 arg == format!(...) 是 &String == String，无对应 PartialEq 实现，编译不过；
            // 该错误此前被 cli_tools/codex.rs 的 #[path] ENOENT 中止掩盖，随 #[path] 修复暴露。
            // .any(|arg| arg == format!("--pid={}", std::process::id())));
            .any(|arg| arg.as_str() == format!("--pid={}", std::process::id())));
    }

    #[test]
    fn linux_gnome_backend_args_identify_app_and_repeat_inhibits() {
        let args = build_linux_gnome_inhibit_args(77, Path::new("/usr/bin/tail"))
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "--app-id".to_string(),
                "AuraCoder".to_string(),
                "--inhibit".to_string(),
                "suspend".to_string(),
                "--inhibit".to_string(),
                "idle".to_string(),
                "--reason".to_string(),
                "Keep system awake while AuraCoder is open".to_string(),
                "/usr/bin/tail".to_string(),
                "--pid=77".to_string(),
                "-f".to_string(),
                "/dev/null".to_string(),
            ]
        );
    }

    #[test]
    fn windows_backend_script_tracks_owner_process_and_prevents_sleep() {
        let profile = PowerProfile::default_profile();
        let args = build_windows_keep_awake_args(77, &profile)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let script = args.last().expect("powershell script should be present");

        assert!(args.iter().any(|arg| arg == "-NoProfile"));
        assert!(args.iter().any(|arg| arg == "-NonInteractive"));
        assert!(args.iter().any(|arg| arg == "-WindowStyle"));
        assert!(args.iter().any(|arg| arg == "Hidden"));
        assert!(script.contains(WINDOWS_KEEP_AWAKE_MARKER));
        assert!(script.contains("$ownerPid = 77"));
        assert!(script.contains("SetThreadExecutionState"));
        assert!(script.contains("Start-Sleep -Seconds 30"));
        assert!(script.contains("$continuous -bor $systemRequired"));
    }

    #[test]
    fn windows_display_required_flag_in_script() {
        let profile = PowerProfile {
            prevent_display_sleep: true,
            ..PowerProfile::default_profile()
        };
        let args = build_windows_keep_awake_args(77, &profile);
        let script = args.last().unwrap().to_string_lossy().into_owned();
        assert!(script.contains("$displayRequired"));
        assert!(script.contains("$continuous -bor $systemRequired -bor $displayRequired"));
    }

    #[test]
    fn windows_screen_saver_disable_in_script() {
        let profile = PowerProfile {
            prevent_screen_saver: true,
            ..PowerProfile::default_profile()
        };
        let args = build_windows_keep_awake_args(77, &profile);
        let script = args.last().unwrap().to_string_lossy().into_owned();
        assert!(script.contains("AuraCoderScreenSaver"));
        assert!(script.contains("SystemParametersInfo"));
        assert!(script.contains("ScreenSaveActive"));
        assert!(script
            .contains("$screenSaverRestoreValue = if ($screenSaverWasActive) { 1 } else { 0 }"));
        assert!(script.contains("SystemParametersInfo(0x11, $screenSaverRestoreValue"));
    }

    #[test]
    fn display_prevention_status_requires_active_display_inhibit() {
        let profile = PowerProfile {
            prevent_display_sleep: true,
            prevent_screen_saver: true,
            ..PowerProfile::default_profile()
        };

        assert_eq!(
            display_prevention_status(true, Some(&profile), false),
            (false, false)
        );
        assert_eq!(
            display_prevention_status(true, Some(&profile), true),
            (true, true)
        );
        assert_eq!(
            display_prevention_status(false, Some(&profile), true),
            (false, false)
        );
    }

    #[test]
    fn profile_from_default_config() {
        let config = PowerConfig::default();
        let profile = PowerProfile::from_config(&config);
        assert!(profile.prevent_system_sleep);
        assert!(!profile.prevent_display_sleep);
        assert!(!profile.prevent_screen_saver);
        assert!(!profile.ac_only);
        assert!(!profile.prevent_closed_display_sleep);
    }

    #[test]
    fn profile_from_full_config() {
        let config = PowerConfig {
            keep_awake_enabled: true,
            prevent_display_sleep: true,
            prevent_screen_saver: true,
            ac_only_mode: true,
            battery_threshold: Some(20),
            session_duration_secs: Some(3600),
            prevent_closed_display_sleep: true,
        };
        let profile = PowerProfile::from_config(&config);
        assert!(profile.prevent_system_sleep);
        assert!(profile.prevent_display_sleep);
        assert!(profile.prevent_screen_saver);
        assert!(profile.ac_only);
        assert!(profile.prevent_closed_display_sleep);
    }

    #[test]
    fn powershell_helper_fingerprint_uses_marker_instead_of_full_script() {
        let program =
            PathBuf::from("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        let profile = PowerProfile::default_profile();
        let args = build_windows_keep_awake_args(88, &profile);
        let fingerprint = helper_command_args_fingerprint(&program, &args);

        assert!(fingerprint.iter().any(|arg| arg == "-NoProfile"));
        assert!(fingerprint
            .iter()
            .any(|arg| arg == WINDOWS_KEEP_AWAKE_MARKER));
        assert!(!fingerprint
            .iter()
            .any(|arg| arg.contains("SetThreadExecutionState")));
    }

    fn exit_status_from_code(code: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            ExitStatus::from_raw(code << 8)
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;

            ExitStatus::from_raw(code as u32)
        }
    }
