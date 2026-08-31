use super::*;
use serde_json::json;

/// 验证工作区内自动模式下，form elicitation 按前端默认字段规则生成接受内容。
#[test]
fn auto_form_response_uses_frontend_default_field_rules() {
    let params = json!({
        "mode": "form",
        "requestedSchema": {
            "properties": {
                "text": {"type": "string"},
                "enabled": {"type": "boolean"},
                "count": {"type": "number"},
                "sequence": {"type": "integer"},
                "items": {"type": "array"},
                "withDefault": {"type": "string", "default": "preset"},
                "nullDefault": {"type": "string", "default": null},
                "invalid": "not-an-object"
            }
        }
    });

    let response =
        build_auto_mcp_elicitation_response(true, "mcpServer/elicitation/request", &params)
            .expect("enabled MCP elicitation should be accepted");

    assert_eq!(
        response,
        json!({
            "action": "accept",
            "content": {
                "text": "",
                "enabled": false,
                "count": 0,
                "sequence": 0,
                "items": [],
                "withDefault": "preset",
                "nullDefault": null,
                "invalid": ""
            }
        })
    );
}

/// 验证 form elicitation 缺少 properties 时仍返回空 content 字段。
#[test]
fn auto_form_response_keeps_empty_content() {
    let response = build_auto_mcp_elicitation_response(
        true,
        "mcpserver/elicitation/request",
        &json!({
            "mode": "form",
            "requested_schema": {}
        }),
    )
    .expect("enabled MCP elicitation should be accepted");

    assert_eq!(response, json!({"action": "accept", "content": {}}));
}

/// 验证 URL 和其他非 form elicitation 只返回接受动作而不附带 content。
#[test]
fn auto_non_form_response_contains_only_accept_action() {
    for mode in ["url", "openaiForm", "unknown"] {
        let response = build_auto_mcp_elicitation_response(
            true,
            "mcpServer/elicitation/request",
            &json!({
                "mode": mode,
                "requestedSchema": {
                    "properties": {"field": {"type": "string"}}
                }
            }),
        )
        .expect("enabled MCP elicitation should be accepted");

        assert_eq!(response, json!({"action": "accept"}));
    }
}

/// 验证非自动模式和非 MCP 请求继续交给既有人工审批映射流程。
#[test]
fn disabled_or_non_mcp_request_does_not_create_auto_response() {
    let params = json!({"mode": "form"});
    assert_eq!(
        build_auto_mcp_elicitation_response(false, "mcpServer/elicitation/request", &params),
        None
    );
    assert_eq!(
        build_auto_mcp_elicitation_response(true, "item/toolCall", &params),
        None
    );
}
