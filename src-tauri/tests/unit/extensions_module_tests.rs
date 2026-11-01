
    use super::validate_extension_id;

    #[test]
    fn rejects_option_and_line_injection_ids() {
        assert!(validate_extension_id("--help").is_err());
        assert!(validate_extension_id("valid\nother").is_err());
        assert!(validate_extension_id("normal@official").is_ok());
    }
