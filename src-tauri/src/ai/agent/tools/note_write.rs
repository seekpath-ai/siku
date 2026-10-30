use async_trait::async_trait;
use sqlx::SqlitePool;
use tauri::Emitter;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::time;

pub struct NoteWriteTool {
    db: SqlitePool,
    /// 写入成功后向前端广播 `note:changed`,让打开的笔记编辑器即时刷新。
    app: Option<tauri::AppHandle>,
    /// Snapshot 截图绝对路径引用在入库前搬进 blob 存储（随笔记同步）。
    app_data_dir: std::path::PathBuf,
}

impl NoteWriteTool {
    pub fn new(db: SqlitePool, app: Option<tauri::AppHandle>, app_data_dir: std::path::PathBuf) -> Self {
        Self { db, app, app_data_dir }
    }

    fn emit_changed(&self, note_id: &str) {
        if let Some(app) = &self.app {
            let _ = app.emit("note:changed", serde_json::json!({ "id": note_id }));
        }
    }
}

#[async_trait]
impl Tool for NoteWriteTool {
    fn name(&self) -> &str { "note_write" }

    fn description(&self) -> &str {
        "Create or update a note. If note_id is provided, updates the existing note (pass the note's id returned by note_read); otherwise creates a new note. Requires approval. Examples — create: note_write(title=\"Meeting notes\", content=\"...\") ; update: note_write(title=\"Meeting notes\", content=\"...\", note_id=\"<note id from note_read>\")"
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "title".into(), param_type: "string".into(),
                description: "Note title".into(), required: true,
            },
            ToolParameter {
                name: "content".into(), param_type: "string".into(),
                description: "Note content in Markdown".into(), required: false,
            },
            ToolParameter {
                name: "note_id".into(), param_type: "string".into(),
                description: "Optional UUID of existing note to update".into(), required: false,
            },
            ToolParameter {
                name: "paper_id".into(), param_type: "string".into(),
                description: "Optional UUID of paper to link this note to".into(), required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let title = args["title"].as_str().unwrap_or("Untitled");
        let raw = args["content"].as_str().unwrap_or("");
        let now = time::now_iso();

        // The agent answers with `[^n]` markers + a trailing ```evidence block.
        // Convert it to the same citation form a 综述 note uses — inline `[n]`
        // links to the evidence passage plus a `## 参考文献` list — so both note
        // pipelines produce the same shape. Papers other than the context paper
        // must name their `paper` id in the evidence entries.
        let note_paper: Option<String> = match args["note_id"].as_str() {
            Some(id) => sqlx::query_scalar::<_, Option<String>>("SELECT paper_id FROM notes WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.db)
                .await
                .ok()
                .flatten()
                .flatten(),
            None => None,
        };
        let default_paper = args["paper_id"].as_str().map(|s| s.to_string()).or(note_paper);
        let content = crate::core::evidence_note::convert(&self.db, raw, default_paper.as_deref())
            .await
            .map_err(|e| format!("引用格式转换失败: {e}"))?;
        let (content, new_blobs) = crate::file_store::ingest_snapshot_image_refs(content.as_str(), &self.app_data_dir);
        crate::sync::attachments::mark_blobs_pending_push(&self.db, &self.app_data_dir, &new_blobs).await;
        let content = content.as_str();

        if let Some(note_id) = args["note_id"].as_str() {
            // Privacy lock: the agent must not modify locked notes.
            if crate::core::note_service::is_note_effectively_locked(&self.db, note_id).await? {
                return Err("该笔记已锁定，无法修改".to_string());
            }
            // Snapshot the current content before the AI edit (version history).
            let old: Option<(String, String)> = sqlx::query_as(
                "SELECT title, content FROM notes WHERE id = ?"
            ).bind(note_id).fetch_optional(&self.db).await.map_err(|e| format!("db error: {e}"))?;
            if let Some((old_title, old_content)) = old {
                sqlx::query(
                    "INSERT INTO note_versions (id, note_id, title, content, edited_by, created_at) VALUES (?, ?, ?, ?, 'agent', ?)"
                ).bind(uuid::Uuid::new_v4().to_string()).bind(note_id).bind(&old_title).bind(&old_content).bind(&now)
                    .execute(&self.db).await.map_err(|e| format!("db error: {e}"))?;
            }

            // Update existing + mark as AI-edited.
            sqlx::query(
                "UPDATE notes SET title = ?, content = ?, content_plain = ?, updated_at = ?, \
                 agent_edited_at = ?, agent_edit_count = agent_edit_count + 1 WHERE id = ?"
            ).bind(title).bind(content).bind(content).bind(&now).bind(&now).bind(note_id)
                .execute(&self.db).await.map_err(|e| format!("db error: {e}"))?;
            self.emit_changed(note_id);
            Ok(format!("Note '{}' updated (id: {})", title, note_id))
        } else {
            // Create new, marked as AI-created/edited. Go through the
            // note_service entry points so the note lands in the right
            // place: literature notes go under 我的图书馆/<集合链>/ (the
            // system library root even when the paper has no collection),
            // plain notes go to the CURRENT vault's root. A raw INSERT
            // here used to skip all of that — notes ended up at the root
            // of the DEFAULT vault regardless of context.
            let vault_id = crate::core::vault_service::get_current_vault_id(&self.db).await?;
            let note = if let Some(paper_id) = args["paper_id"].as_str() {
                crate::core::note_service::create_note_under_paper(
                    &self.db, paper_id, title, content, &vault_id,
                ).await?
            } else {
                crate::core::note_service::create_note(
                    &self.db, title, content, None, None, &vault_id, false,
                ).await?
            };
            sqlx::query(
                "UPDATE notes SET agent_edited_at = ?, agent_edit_count = 1, updated_at = ? WHERE id = ?"
            ).bind(&now).bind(&now).bind(&note.id)
                .execute(&self.db).await.map_err(|e| format!("db error: {e}"))?;
            self.emit_changed(&note.id);
            Ok(format!("Note '{}' created (id: {})", title, note.id))
        }
    }
}
