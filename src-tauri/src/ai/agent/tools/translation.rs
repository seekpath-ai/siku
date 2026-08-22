use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

pub struct TranslationTool {
    db: SqlitePool,
}

impl TranslationTool {
    pub fn new(db: SqlitePool) -> Self { Self { db } }
}

#[async_trait]
impl Tool for TranslationTool {
    fn name(&self) -> &str { "translate" }

    fn readonly(&self) -> bool { true }

    fn description(&self) -> &str {
        "Translate text between languages. Use this to help users translate content. Default target is Chinese (zh)."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "text".into(), param_type: "string".into(),
                description: "Text to translate".into(), required: true,
            },
            ToolParameter {
                name: "target_lang".into(), param_type: "string".into(),
                description: "Target language code (zh, en, ja, ko, fr, de). Default: zh".into(), required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let text = args["text"].as_str().ok_or("text required")?;
        let target = args["target_lang"].as_str().unwrap_or("zh");

        crate::ai::translation::service::translate_text(&self.db, text, None, Some(target)).await
    }
}
