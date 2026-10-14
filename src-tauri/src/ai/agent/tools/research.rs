//! research_topic_create: the agent creates a research-tracking topic
//! (科研追踪). The topic then flows through the normal discovery pipeline
//! (arXiv + Crossref + Semantic Scholar, LLM relevance scoring).

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// Create a research topic. Write tool (subject to approval mode).
pub struct ResearchTopicCreateTool {
    db: SqlitePool,
}

impl ResearchTopicCreateTool {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Tool for ResearchTopicCreateTool {
    fn name(&self) -> &str {
        "research_topic_create"
    }

    fn description(&self) -> &str {
        "Create a research-tracking topic (科研追踪): the app then periodically discovers new papers for it from arXiv/Crossref/Semantic Scholar with relevance scoring. Use when the user asks to track a research direction. If the request is vague, first clarify the scope with ask_user, then create with precise English search keywords. Do not create duplicate topics — if a similar topic exists, tell the user instead."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "name".into(),
                param_type: "string".into(),
                description: "Topic name (Chinese is fine)".into(),
                required: true,
            },
            ToolParameter {
                name: "keywords".into(),
                param_type: "string".into(),
                description: "Comma-separated ENGLISH search keywords/phrases for literature search, e.g. \"slope stability,graph neural network\"".into(),
                required: true,
            },
            ToolParameter {
                name: "description".into(),
                param_type: "string".into(),
                description: "Optional: one or two sentences on scope and focus".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let name = args["name"].as_str().unwrap_or("").trim();
        if name.is_empty() {
            return Err("name is empty".to_string());
        }
        let keywords: Vec<String> = args["keywords"]
            .as_str()
            .unwrap_or("")
            .split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        if keywords.is_empty() {
            return Err("keywords is empty (comma-separated English search terms required)".to_string());
        }
        let description = args["description"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        // Duplicate guard: same name (case-insensitive) already exists.
        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM research_topics WHERE LOWER(name) = LOWER(?) LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| format!("db: {e}"))?;
        if existing.is_some() {
            return Err(format!("已存在同名课题「{name}」，未重复创建。如需调整关键词请在科研追踪页编辑该课题。"));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = crate::core::time::now_iso();
        let keywords_json = serde_json::to_string(&keywords).map_err(|e| format!("json: {e}"))?;
        sqlx::query(
            "INSERT INTO research_topics (id, name, description, keywords, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, 'active', ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(&description)
        .bind(&keywords_json)
        .bind(&now)
        .bind(&now)
        .execute(&self.db)
        .await
        .map_err(|e| format!("db: {e}"))?;

        Ok(format!(
            "已创建科研追踪课题「{name}」（关键词：{}）。可在科研追踪页点击「发现」立即搜索，或等待自动发现。",
            keywords.join(", ")
        ))
    }
}
