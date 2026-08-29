//! AuraCoder 本地会话读取工具服务。
//!
//! 该服务只访问 AuraCoder 自己的 SQLite 数据库，不依赖 CUA，也不访问 CLI
//! 远端会话。各本机引擎在创建/发送线程时登记来源工作区，工具调用只能
//! 读取同一工作区中的线程。

use std::{collections::HashMap, sync::Mutex};

use serde_json::{json, Value};

use crate::{
    db::{messages, threads, Database},
    models::ThreadDto,
};

/// AuraCoder 会话 MCP 的命名空间。
pub const PANES_THREAD_NAMESPACE: &str = "auracoder_thread";

/// 会话 MCP 工具服务。
pub struct AuraCoderThreadMcpService {
    /// AuraCoder 主数据库连接池。
    db: Database,
    /// 引擎线程到 AuraCoder 工作区的来源登记。
    bindings: Mutex<HashMap<(String, String), String>>,
}

impl AuraCoderThreadMcpService {
    /// 创建绑定到指定 AuraCoder 数据库的会话工具服务。
    pub fn new(db: Database) -> Self {
        Self {
            db,
            bindings: Mutex::new(HashMap::new()),
        }
    }

    /// 返回 MCP 暴露的两个且仅两个工具规格。
    pub fn tool_specs(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "get_auracoder_thread_message_count",
                "description": "获取指定 AuraCoder 会话的消息总行数。回答前必须先使用此工具确定分页范围。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "thread_id": { "type": "string", "description": "AuraCoder 会话 ID" }
                    },
                    "required": ["thread_id"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "get_auracoder_thread_messages_page",
                "description": "按创建时间倒序分页读取指定 AuraCoder 会话消息。page 和 page_size 从 1 开始。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "thread_id": { "type": "string", "description": "AuraCoder 会话 ID" },
                        "page": { "type": "integer", "minimum": 1, "description": "页码，从 1 开始" },
                        "page_size": { "type": "integer", "minimum": 1, "description": "每页条数，从 1 开始" }
                    },
                    "required": ["thread_id", "page", "page_size"],
                    "additionalProperties": false
                }
            }),
        ]
    }

    /// 返回 Codex dynamicTools 所需的会话工具命名空间。
    pub fn dynamic_tools_spec(&self) -> Value {
        let tools = self
            .tool_specs()
            .into_iter()
            .map(|spec| {
                json!({
                    "type": "function",
                    "name": spec["name"].clone(),
                    "description": spec["description"].clone(),
                    "inputSchema": spec["inputSchema"].clone(),
                })
            })
            .collect::<Vec<_>>();
        json!([{
            "type": "namespace",
            "name": PANES_THREAD_NAMESPACE,
            "description": "读取 AuraCoder 当前项目中的本机会话内容。",
            "tools": tools
        }])
    }

    /// 登记引擎线程所属的 AuraCoder 工作区。
    pub fn bind_engine_thread(&self, engine_id: &str, engine_thread_id: &str, workspace_id: &str) {
        let engine_id = engine_id.trim();
        let engine_thread_id = engine_thread_id.trim();
        let workspace_id = workspace_id.trim();
        if engine_id.is_empty() || engine_thread_id.is_empty() || workspace_id.is_empty() {
            return;
        }
        if let Ok(mut bindings) = self.bindings.lock() {
            bindings.insert(
                (engine_id.to_string(), engine_thread_id.to_string()),
                workspace_id.to_string(),
            );
        }
    }

    /// 查询可信引擎线程登记的 AuraCoder 项目范围。
    pub fn workspace_for_engine_thread(
        &self,
        engine_id: &str,
        engine_thread_id: &str,
    ) -> Result<String, String> {
        let bindings = self
            .bindings
            .lock()
            .map_err(|_| "AuraCoder 会话工具来源登记不可用".to_string())?;
        bindings
            .get(&(engine_id.trim().to_string(), engine_thread_id.trim().to_string()))
            .cloned()
            .ok_or_else(|| "当前引擎线程尚未登记 AuraCoder 项目范围".to_string())
    }

    /// 读取目标 AuraCoder 会话所属的项目，不判断调用方是否有权访问。
    pub async fn thread_workspace(&self, thread_id: &str) -> Result<Option<String>, String> {
        let db = self.db.clone();
        let thread_id = thread_id.trim().to_string();
        tokio::task::spawn_blocking(move || {
            threads::get_thread(&db, &thread_id)
                .map_err(|error| error.to_string())
                .map(|thread| thread.map(|value| value.workspace_id))
        })
        .await
        .map_err(|error| format!("AuraCoder 会话项目查询任务失败：{error}"))?
    }

    /// 读取目标 AuraCoder 会话的消息数量，不判断调用方是否有权访问。
    pub async fn thread_message_count(&self, thread_id: &str) -> Result<Value, String> {
        let db = self.db.clone();
        let thread_id = thread_id.trim().to_string();
        tokio::task::spawn_blocking(move || {
            let message_count = messages::count_thread_messages(&db, &thread_id)
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "thread_id": thread_id,
                "message_count": message_count
            }))
        })
        .await
        .map_err(|error| format!("AuraCoder 会话消息数量查询任务失败：{error}"))?
    }

    /// 按创建时间倒序分页读取目标 AuraCoder 会话消息，不判断调用方是否有权访问。
    pub async fn thread_messages_page(
        &self,
        thread_id: &str,
        page: u64,
        page_size: u64,
    ) -> Result<Value, String> {
        let db = self.db.clone();
        let thread_id = thread_id.trim().to_string();
        tokio::task::spawn_blocking(move || {
            let records = messages::get_thread_messages_page_desc(&db, &thread_id, page, page_size)
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "thread_id": thread_id,
                "page": page,
                "page_size": page_size,
                "messages": records
            }))
        })
        .await
        .map_err(|error| format!("AuraCoder 会话消息分页查询任务失败：{error}"))?
    }

    /// 按引擎来源读取一个会话工具。
    pub async fn invoke_for_engine(
        &self,
        engine_id: &str,
        engine_thread_id: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let source_workspace = {
            let bindings = self
                .bindings
                .lock()
                .map_err(|_| "AuraCoder 会话工具来源登记不可用".to_string())?;
            bindings
                .get(&(
                    engine_id.trim().to_string(),
                    engine_thread_id.trim().to_string(),
                ))
                .cloned()
                .ok_or_else(|| "当前引擎线程尚未登记 AuraCoder 项目范围".to_string())?
        };
        let args = arguments
            .as_object()
            .ok_or_else(|| "AuraCoder 会话工具 arguments 必须是对象".to_string())?;
        let target_thread_id = args
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "缺少必填参数 thread_id".to_string())?
            .to_string();
        let page = args.get("page").and_then(Value::as_u64);
        let page_size = args.get("page_size").and_then(Value::as_u64);

        let db = self.db.clone();
        let source_workspace_for_query = source_workspace.clone();
        let tool = tool.trim().to_string();
        tokio::task::spawn_blocking(move || {
            let target = threads::get_thread(&db, &target_thread_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "指定的 AuraCoder 会话不存在".to_string())?;
            ensure_workspace(&target, &source_workspace_for_query)?;
            match tool.as_str() {
                "get_auracoder_thread_message_count" => {
                    let message_count = messages::count_thread_messages(&db, &target_thread_id)
                        .map_err(|error| error.to_string())?;
                    Ok(json!({
                        "thread_id": target_thread_id,
                        "message_count": message_count
                    }))
                }
                "get_auracoder_thread_messages_page" => {
                    let page = page.ok_or_else(|| "缺少必填参数 page".to_string())?;
                    let page_size =
                        page_size.ok_or_else(|| "缺少必填参数 page_size".to_string())?;
                    let records = messages::get_thread_messages_page_desc(
                        &db,
                        &target_thread_id,
                        page,
                        page_size,
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(json!({
                        "thread_id": target_thread_id,
                        "page": page,
                        "page_size": page_size,
                        "messages": records
                    }))
                }
                _ => Err(format!("未知的 AuraCoder 会话工具：{tool}")),
            }
        })
        .await
        .map_err(|error| format!("AuraCoder 会话工具查询任务失败：{error}"))?
    }
}

/// 验证被读取会话与引擎来源属于同一 AuraCoder 工作区。
fn ensure_workspace(thread: &ThreadDto, source_workspace: &str) -> Result<(), String> {
    if thread.workspace_id != source_workspace {
        return Err("AuraCoder 会话工具只允许读取当前项目的会话".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn exposes_exactly_two_tools() {
        let db = test_database();
        let service = AuraCoderThreadMcpService::new(db);
        let names = service
            .tool_specs()
            .into_iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "get_auracoder_thread_message_count",
                "get_auracoder_thread_messages_page",
            ]
        );
    }

    #[test]
    fn serializes_page_message_timestamp_as_snake_case() {
        let record = messages::AuraCoderThreadMessageRecord {
            id: "message-1".to_string(),
            role: "user".to_string(),
            content: Some("内容".to_string()),
            created_at: "2026-08-22T00:00:00Z".to_string(),
        };
        let value = serde_json::to_value(record).expect("message record should serialize");
        assert_eq!(value["created_at"], "2026-08-22T00:00:00Z");
        assert!(value.get("createdAt").is_none());
    }

    #[tokio::test]
    async fn reads_count_and_descending_pages_with_workspace_isolation() {
        let db = test_database();
        let workspace_one = crate::db::workspaces::upsert_workspace(
            &db,
            &std::env::temp_dir()
                .join(format!(
                    "auracoder-thread-mcp-workspace-one-{}",
                    uuid::Uuid::new_v4()
                ))
                .to_string_lossy(),
        )
        .expect("first workspace");
        let workspace_two = crate::db::workspaces::upsert_workspace(
            &db,
            &std::env::temp_dir()
                .join(format!(
                    "auracoder-thread-mcp-workspace-two-{}",
                    uuid::Uuid::new_v4()
                ))
                .to_string_lossy(),
        )
        .expect("second workspace");
        let target_thread = crate::db::threads::create_thread(
            &db,
            &workspace_one.id,
            "claude",
            "model",
            "Referenced thread",
        )
        .expect("target thread");
        let other_thread = crate::db::threads::create_thread(
            &db,
            &workspace_two.id,
            "opencode",
            "model",
            "Other thread",
        )
        .expect("other thread");

        let early = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "早",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("early message");
        let middle = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "中",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("middle message");
        let late = crate::db::messages::insert_user_message(
            &db,
            &target_thread.id,
            "晚",
            None,
            Some("claude"),
            Some("model"),
            None,
        )
        .expect("late message");
        {
            let conn = db.connect().expect("database connection");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:01Z", early.id],
            )
            .expect("early timestamp");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:02Z", middle.id],
            )
            .expect("middle timestamp");
            conn.execute(
                "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                params!["2026-08-22T00:00:03Z", late.id],
            )
            .expect("late timestamp");
        }

        let service = AuraCoderThreadMcpService::new(db);
        service.bind_engine_thread("codex", "current-engine-thread", &workspace_one.id);
        let count = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_message_count",
                json!({ "thread_id": target_thread.id }),
            )
            .await
            .expect("count query");
        assert_eq!(count["message_count"], 3);

        let first_page = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_messages_page",
                json!({ "thread_id": target_thread.id, "page": 1, "page_size": 2 }),
            )
            .await
            .expect("first page query");
        let first_messages = first_page["messages"]
            .as_array()
            .expect("first page messages");
        assert_eq!(first_messages.len(), 2);
        assert_eq!(first_messages[0]["content"], "晚");
        assert_eq!(first_messages[1]["content"], "中");
        for message in first_messages {
            assert!(message.get("id").is_some());
            assert!(message.get("role").is_some());
            assert!(message.get("content").is_some());
            assert!(message.get("created_at").is_some());
            assert!(message.get("createdAt").is_none());
        }

        let second_page = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_messages_page",
                json!({ "thread_id": target_thread.id, "page": 2, "page_size": 2 }),
            )
            .await
            .expect("second page query");
        assert_eq!(second_page["messages"][0]["content"], "早");

        let isolation_error = service
            .invoke_for_engine(
                "codex",
                "current-engine-thread",
                "get_auracoder_thread_message_count",
                json!({ "thread_id": other_thread.id }),
            )
            .await
            .expect_err("cross-workspace query must fail");
        assert_eq!(
            isolation_error,
            "AuraCoder 会话工具只允许读取当前项目的会话"
        );
    }

    fn test_database() -> Database {
        let path =
            std::env::temp_dir().join(format!("auracoder-thread-mcp-{}.db", uuid::Uuid::new_v4()));
        Database::open(path).expect("test database")
    }
}
