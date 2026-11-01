
    use super::*;

    #[test]
    fn dto_from_runtime_preserves_closed_display_state() {
        let dto = dto_from_runtime(
            KeepAwakeStatus {
                supported: true,
                active: true,
                supports_closed_display: Some(false),
                closed_display_active: Some(false),
                message: Some("limited".to_string()),
                display_sleep_prevented: true,
                screen_saver_prevented: false,
                on_ac_power: Some(true),
                battery_percent: Some(87),
                session_remaining_secs: Some(1800),
                paused_due_to_battery: false,
                closed_display_sleep_disabled: false,
            },
            true,
        );

        assert!(dto.supported);
        assert!(dto.enabled);
        assert!(dto.active);
        assert_eq!(dto.supports_closed_display, Some(false));
        assert_eq!(dto.closed_display_active, Some(false));
        assert_eq!(dto.message.as_deref(), Some("limited"));
        assert!(dto.display_sleep_prevented);
        assert!(!dto.screen_saver_prevented);
        assert_eq!(dto.on_ac_power, Some(true));
        assert_eq!(dto.battery_percent, Some(87));
        assert_eq!(dto.session_remaining_secs, Some(1800));
        assert!(!dto.paused_due_to_battery);
    }
