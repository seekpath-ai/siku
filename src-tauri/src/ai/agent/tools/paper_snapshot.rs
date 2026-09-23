use async_trait::async_trait;
use sqlx::SqlitePool;
use std::path::PathBuf;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::ai::llm::{self, ImagePart};
use crate::pdf::figures::{self, FigureKind};

/// Render DPI for snapshots: high enough that axis labels and table text stay
/// legible for the vision model and the user's zoomed view.
const SNAPSHOT_DPI: f32 = 250.0;
/// Padding (PDF points) around a matched figure bbox so tick labels and
/// caption-adjacent marks are not cut off.
const BBOX_PAD: f32 = 8.0;

/// Render a region of a paper's PDF page to a PNG the user can see.
pub struct PaperSnapshotTool {
    db: SqlitePool,
    app_data_dir: PathBuf,
    vision_llm: Option<llm::LlmConfig>,
}

impl PaperSnapshotTool {
    pub fn new(db: SqlitePool, app_data_dir: PathBuf, vision_llm: Option<llm::LlmConfig>) -> Self {
        Self {
            db,
            app_data_dir,
            vision_llm,
        }
    }
}

/// One paper_figures row as the tool needs it.
struct FigureRow {
    id: String,
    page: i32,
    kind: FigureKind,
    label: String,
    caption: String,
    bbox: Option<[f32; 4]>,
    caption_bbox: Option<[f32; 4]>,
}

fn parse_bbox_json(s: Option<&str>) -> Option<[f32; 4]> {
    serde_json::from_str::<[f32; 4]>(s?).ok()
}

/// Filename-safe slug for a label/region: keeps CJK and alphanumerics.
fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "snap".to_string()
    } else {
        trimmed.chars().take(40).collect()
    }
}

#[async_trait]
impl Tool for PaperSnapshotTool {
    fn name(&self) -> &str {
        "paper_snapshot"
    }

    fn readonly(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Render a region of a paper's PDF page to a PNG image and return its path, so the user \
         can see the paper's actual figures/tables. Locate the region by exactly one of: \
         `label` — a figure/table caption label like \"图3\", \"Fig. 2\" or \"Table 1\" (figure \
         metadata is indexed at import time; preferred); `rect` — an explicit crop \"x0,y0,x1,y1\" \
         in PDF points (y-up); or `region` — top/middle/bottom/full page thirds (default full). \
         IMPORTANT: after calling this tool you MUST embed the returned path in your reply as \
         ![](path) so the user sees the image in the chat bubble. For tables, prefer quoting the \
         data as a markdown text table (copyable) and attach the image only when the layout is \
         complex. Set analyze=true to have the configured vision (multimodal) model describe the \
         captured image — axes, data trends, conclusions — and return that description."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "paper_id".into(),
                param_type: "string".into(),
                description: "The UUID of the paper (same as paper_read)".into(),
                required: true,
            },
            ToolParameter {
                name: "page".into(),
                param_type: "integer".into(),
                description: "1-based page number. Optional when `label` is given (the label is then searched across all pages); defaults to 1".into(),
                required: false,
            },
            ToolParameter {
                name: "label".into(),
                param_type: "string".into(),
                description: "Caption label of the figure/table to capture, e.g. \"图3\", \"Fig. 2\", \"Table 1\"".into(),
                required: false,
            },
            ToolParameter {
                name: "rect".into(),
                param_type: "string".into(),
                description: "Explicit crop rectangle \"x0,y0,x1,y1\" in PDF points (y-up, origin bottom-left)".into(),
                required: false,
            },
            ToolParameter {
                name: "region".into(),
                param_type: "string".into(),
                description: "Region hint when neither label nor rect is given: top | middle | bottom | full (default full)".into(),
                required: false,
            },
            ToolParameter {
                name: "analyze".into(),
                param_type: "boolean".into(),
                description: "Also analyze the captured PNG with the configured vision model and return its description (default false)".into(),
                required: false,
            },
            ToolParameter {
                name: "analyze_prompt".into(),
                param_type: "string".into(),
                description: "Custom instruction for the vision analysis (used only with analyze=true)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let paper_id = args["paper_id"].as_str().ok_or("paper_id required")?;
        let page_arg = args["page"].as_i64().map(|p| p.max(1) as u16);
        let label = args["label"].as_str().map(str::trim).filter(|s| !s.is_empty());
        let rect_arg = args["rect"].as_str();
        let region = args["region"].as_str().unwrap_or("full");
        let analyze = args["analyze"].as_bool().unwrap_or(false);

        let paper: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT title, file_path FROM papers WHERE id = ?",
        )
        .bind(paper_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| format!("db error: {e}"))?;
        let (title, file_path) =
            paper.ok_or_else(|| format!("paper not found: {paper_id}"))?;
        let rel_path = file_path.ok_or("该文献没有关联的 PDF 文件")?;
        let pdf_path = crate::file_store::resolve_blob_path(&self.app_data_dir, &rel_path);
        if !pdf_path.exists() {
            return Err(format!("PDF 文件不存在: {}", pdf_path.display()));
        }

        // Resolve the crop rect (display-frame PDF points, y-up) and page.
        let mut matched_figure: Option<FigureRow> = None;
        let (page, rect): (u16, [f32; 4]) = if let Some(lbl) = label {
            let want = figures::normalize_label(lbl);
            let rows: Vec<(String, i32, String, String, String, Option<String>, Option<String>)> =
                sqlx::query_as(
                    "SELECT id, page, kind, label, caption, bbox, caption_bbox FROM paper_figures WHERE paper_id = ? ORDER BY page",
                )
                .bind(paper_id)
                .fetch_all(&self.db)
                .await
                .map_err(|e| format!("db error: {e}"))?;
            let found = rows
                .iter()
                .filter(|r| !r.3.is_empty() && figures::normalize_label(&r.3) == want)
                .filter(|r| page_arg.map(|p| r.1 == p as i32).unwrap_or(true))
                .map(|r| FigureRow {
                    id: r.0.clone(),
                    page: r.1,
                    kind: FigureKind::from_str(&r.2),
                    label: r.3.clone(),
                    caption: r.4.clone(),
                    bbox: parse_bbox_json(r.5.as_deref()),
                    caption_bbox: parse_bbox_json(r.6.as_deref()),
                })
                .next();
            let Some(fig) = found else {
                let available: Vec<String> = rows
                    .iter()
                    .filter(|r| !r.3.is_empty())
                    .map(|r| format!("{} (p.{})", r.3, r.1))
                    .collect();
                return Ok(format!(
                    "No figure/table labelled \"{lbl}\" found in the index of this paper{}.\n\
                     Indexed labels: {}\n\
                     (Labels are collected when the paper is indexed — re-index the paper if this list is empty. \
                     You can still snapshot by page + region/rect.)",
                    page_arg.map(|p| format!(" on page {p}")).unwrap_or_default(),
                    if available.is_empty() { "(none)".into() } else { available.join(", ") },
                ));
            };
            let (pw, ph) = figures::page_size(&pdf_path, (fig.page - 1) as u16)
                .map_err(|e| format!("page size: {e}"))?;
            let rect = if let Some(b) = fig.bbox {
                figures::pad_rect(b, BBOX_PAD, pw, ph)
            } else if let Some(cb) = fig.caption_bbox {
                figures::infer_rect_from_caption(fig.kind, cb, pw, ph)
            } else {
                figures::region_rect("full", pw, ph).unwrap()
            };
            let page = fig.page as u16;
            matched_figure = Some(fig);
            (page, rect)
        } else if let Some(r) = rect_arg {
            let page = page_arg.unwrap_or(1);
            let rect = figures::parse_rect(r)
                .ok_or_else(|| format!("invalid rect \"{r}\" — expected \"x0,y0,x1,y1\" in PDF points"))?;
            (page, rect)
        } else {
            let page = page_arg.unwrap_or(1);
            let (pw, ph) = figures::page_size(&pdf_path, page - 1)
                .map_err(|e| format!("page size: {e}"))?;
            let rect = figures::region_rect(region, pw, ph).ok_or_else(|| {
                format!("invalid region \"{region}\" — expected top | middle | bottom | full")
            })?;
            (page, rect)
        };

        // Render + crop.
        let dir = self.app_data_dir.join("snapshots");
        let name = format!(
            "{}_p{}_{}.png",
            &paper_id[..paper_id.len().min(8)],
            page,
            slug(label.unwrap_or(region))
        );
        let out_path = dir.join(&name);
        let (width, height) = tokio::task::spawn_blocking({
            let pdf_path = pdf_path.clone();
            let out_path = out_path.clone();
            move || {
                crate::pdf::renderer::render_page_region(
                    &pdf_path,
                    page - 1,
                    rect,
                    SNAPSHOT_DPI,
                    &out_path,
                )
            }
        })
        .await
        .map_err(|e| format!("render task: {e}"))?
        .map_err(|e| format!("render failed: {e}"))?;

        // Record the rendered file on the matched figure row (best-effort).
        if let Some(fig) = &matched_figure {
            let _ = sqlx::query("UPDATE paper_figures SET image_path = ? WHERE id = ?")
                .bind(out_path.to_string_lossy().to_string())
                .bind(&fig.id)
                .execute(&self.db)
                .await;
        }

        let mut info = serde_json::json!({
            "path": out_path.to_string_lossy(),
            "paper": title,
            "page": page,
            "width": width,
            "height": height,
        });
        if let Some(fig) = &matched_figure {
            info["label"] = serde_json::json!(fig.label);
            info["kind"] = serde_json::json!(fig.kind.as_str());
            if !fig.caption.is_empty() {
                info["caption"] = serde_json::json!(fig.caption.chars().take(200).collect::<String>());
            }
            if fig.bbox.is_none() {
                info["note"] = serde_json::json!(
                    "no image object was indexed for this caption (vector figure or drawn table); \
                     the crop was inferred from the caption position — verify it covers the content \
                     and retry with region/rect if not"
                );
            }
        }

        let mut result = format!(
            "{info}\n\n\
             截图已保存。**在回复正文中用 ![]({}) 嵌入此图**，用户即可在气泡中查看论文原图。\
             表格内容优先整理为 markdown 文本表格（可复制），版式复杂时再附图。",
            out_path.to_string_lossy()
        );

        if analyze {
            result.push_str(&self.analyze_snapshot(&out_path, label, args["analyze_prompt"].as_str()).await);
        }
        Ok(result)
    }
}

impl PaperSnapshotTool {
    /// One-shot vision call on the rendered PNG (Step 4, approach (a): a
    /// backend-side call that never touches the agent loop). Gracefully
    /// degrades to a note when no vision model is configured.
    async fn analyze_snapshot(
        &self,
        png_path: &std::path::Path,
        label: Option<&str>,
        custom_prompt: Option<&str>,
    ) -> String {
        let Some(cfg) = &self.vision_llm else {
            return "\n\n[图像理解] 未为本智能体配置多模态（视觉）模型，已跳过 analyze；\
                    图片本身仍可正常展示。可在会话设置中选择视觉模型后重试。"
                .to_string();
        };
        let bytes = match std::fs::read(png_path) {
            Ok(b) => b,
            Err(e) => return format!("\n\n[图像理解] 读取截图失败: {e}"),
        };
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &bytes,
        );
        let prompt = custom_prompt.unwrap_or(
            "这是学术论文中的一张图或表。请描述其内容与结论：若是图，说明图的类型、坐标轴含义、\
             主要数据趋势与结论；若是表，请以 markdown 表格转录其主要数据。用中文回答。",
        );
        let prompt = match label {
            Some(l) => format!("（该截图对应论文中的「{l}」。）\n{prompt}"),
            None => prompt.to_string(),
        };
        let client = match llm::client::create_llm_client(cfg) {
            Ok(c) => c,
            Err(e) => return format!("\n\n[图像理解] 视觉模型客户端创建失败: {e}"),
        };
        let image = ImagePart {
            mime: "image/png".to_string(),
            base64: b64,
        };
        match client
            .chat_completion_vision(
                "You are an expert at reading academic figures and tables.",
                &prompt,
                &[image],
            )
            .await
        {
            Ok(resp) => format!("\n\n[视觉模型解读]\n{}", resp.content),
            Err(e) => format!("\n\n[图像理解] 视觉模型调用失败: {e}（图片本身已保存，仍可展示）"),
        }
    }
}
