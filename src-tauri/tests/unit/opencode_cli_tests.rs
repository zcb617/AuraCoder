use super::{CliUsageQueryScope, OpenCodeCli};

/// 验证 OpenCode 只在用户显式选择时返回不可用额度结果。
#[test]
fn usage_summary_omits_opencode_but_explicit_query_reports_unavailable() {
    assert!(OpenCodeCli::opencode_provider_usage(CliUsageQueryScope::Summary).is_none());

    let usage = OpenCodeCli::opencode_provider_usage(CliUsageQueryScope::Explicit)
        .expect("explicit OpenCode usage query should return an unavailable result");
    assert_eq!(usage.engine_id, "opencode");
    assert!(!usage.available);
    assert!(usage.windows.is_empty());
}
