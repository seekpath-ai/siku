use std::path::PathBuf;
use async_trait::async_trait;
use sqlx::SqlitePool;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

pub struct PaperImportTool {
    db: SqlitePool,
    app_data_dir: PathBuf,
}

impl PaperImportTool {
    pub fn new(db: SqlitePool, app_data_dir: PathBuf) -> Self { Self { db, app_data_dir } }
}

#[async_trait]
impl Tool for PaperImportTool {
    fn name(&self) -> &str { "paper_import" }

    fn description(&self) -> &str {
        "Import a paper from a local file path. Use this when the user wants to add a PDF to their library. The file must exist on disk. Requires approval."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "file_path".into(),
            param_type: "string".into(),
            description: "Absolute path to the PDF file on the local filesystem".into(),
            required: true,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let file_path = args["file_path"].as_str().ok_or("file_path required")?;
        let path = std::path::Path::new(file_path);

        if !path.exists() {
            return Ok(format!("File not found: {file_path}"));
        }

        match crate::core::paper_service::import_paper(&self.db, &self.app_data_dir, path).await {
            Ok(paper) => {
                Ok(format!(
                    "Successfully imported: **{}** ({})\nAuthors: {}\nPages: {}",
                    paper.title,
                    paper.year.unwrap_or(0),
                    paper.authors,
                    paper.page_count.unwrap_or(0),
                ))
            }
            Err(e) => Ok(format!("Failed to import paper: {e}")),
        }
    }
}
