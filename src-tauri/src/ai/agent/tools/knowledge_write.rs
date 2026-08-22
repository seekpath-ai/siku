use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::time;

pub struct KnowledgeWriteTool {
    db: SqlitePool,
}

impl KnowledgeWriteTool {
    pub fn new(db: SqlitePool) -> Self { Self { db } }
}

#[async_trait]
impl Tool for KnowledgeWriteTool {
    fn name(&self) -> &str { "knowledge_create" }
    fn description(&self) -> &str {
        "Create a knowledge item in the user's knowledge base. Use this to save important information, notes, or findings to a specific domain. Domains: research, learning, life, reading, notes. Requires approval."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter { name: "title".into(), param_type: "string".into(), description: "Title of the knowledge item".into(), required: true },
            ToolParameter { name: "content".into(), param_type: "string".into(), description: "Content in markdown format".into(), required: false },
            ToolParameter { name: "domain".into(), param_type: "string".into(), description: "Domain type: research, learning, life, reading, or notes. Default: notes".into(), required: false },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let title = args["title"].as_str().unwrap_or("Untitled");
        let content = args["content"].as_str().unwrap_or("");
        let domain_type = args["domain"].as_str().unwrap_or("notes");

        // Find domain_id by domain_type
        let domain_id: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM knowledge_domains WHERE domain_type = ? LIMIT 1"
        ).bind(domain_type).fetch_optional(&self.db).await.map_err(|e| format!("db: {e}"))?;

        let did = match domain_id {
            Some((id,)) => id,
            None => "dom-notes".to_string(),
        };

        let id = uuid::Uuid::new_v4().to_string();
        let now = time::now_iso();

        sqlx::query(
            "INSERT INTO knowledge_items (id, domain_id, title, content_type, content, tags, metadata, created_at, updated_at) VALUES (?, ?, ?, 'note', ?, '[]', '{}', ?, ?)"
        ).bind(&id).bind(&did).bind(title).bind(content).bind(&now).bind(&now)
         .execute(&self.db).await.map_err(|e| format!("db: {e}"))?;

        Ok(format!("Knowledge item '{}' created in the {} domain (id: {})", title, domain_type, id))
    }
}
