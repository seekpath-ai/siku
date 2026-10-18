use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::path::PathBuf;

use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::ai::llm::{self, ImagePart};
use crate::pdf::figures::{self, FigureKind};

const SNAPSHOT_DPI: f32 = 250.0;
const LOCATION_DPI: f32 = 160.0;
const BBOX_PAD: f32 = 8.0;
const VISION_REFINE_BELOW: f32 = 0.75;

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

struct FigureRow {
    id: String,
    page: i32,
    kind: FigureKind,
    label: String,
    caption: String,
    bbox: Option<[f32; 4]>,
    caption_bbox: Option<[f32; 4]>,
}

#[derive(Debug, Deserialize)]
struct VisionLocation {
    bbox: [f32; 4],
    confidence: Option<f32>,
}

fn parse_bbox_json(s: Option<&str>) -> Option<[f32; 4]> {
    serde_json::from_str::<[f32; 4]>(s?).ok()
}

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

fn display_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s
    }
}

fn validate_selectors(
    label: Option<&str>,
    rect: Option<&str>,
    region: Option<&str>,
) -> Result<(), String> {
    let count =
        usize::from(label.is_some()) + usize::from(rect.is_some()) + usize::from(region.is_some());
    if count > 1 {
        Err("label, rect and region are mutually exclusive; pass only one".into())
    } else {
        Ok(())
    }
}

fn snapshot_filename(paper_id: &str, page: u16, hint: &str, rect: [f32; 4]) -> String {
    let digest = Sha256::digest(format!("{page}|{rect:?}|{SNAPSHOT_DPI}").as_bytes());
    let hash = hex::encode(digest);
    format!(
        "{}_p{}_{}_{}.png",
        &paper_id[..paper_id.len().min(8)],
        page,
        slug(hint),
        &hash[..12]
    )
}

fn confidence_label(score: f32) -> &'static str {
    if score >= 0.8 {
        "high"
    } else if score >= 0.6 {
        "medium"
    } else {
        "low"
    }
}

fn parse_vision_location(raw: &str) -> Result<(VisionLocation, f32), String> {
    let start = raw
        .find('{')
        .ok_or("vision location returned no JSON object")?;
    let end = raw
        .rfind('}')
        .ok_or("vision location returned no JSON object end")?;
    if end < start {
        return Err("vision location returned malformed JSON".into());
    }
    let parsed: VisionLocation = serde_json::from_str(&raw[start..=end])
        .map_err(|e| format!("invalid vision location JSON: {e}"))?;
    if !parsed.bbox.iter().all(|v| v.is_finite()) {
        return Err("vision location contains non-finite coordinates".into());
    }
    let confidence = parsed.confidence.unwrap_or(0.7).clamp(0.0, 1.0);
    if confidence < 0.5 {
        return Err(format!(
            "vision location confidence is too low: {confidence:.2}"
        ));
    }
    Ok((parsed, confidence))
}

fn normalized_vision_rect(
    location: &VisionLocation,
    page_w: f32,
    page_h: f32,
    caption_bbox: Option<[f32; 4]>,
) -> Result<[f32; 4], String> {
    let b = location.bbox;
    let left = b[0].clamp(0.0, 1.0);
    let top = b[1].clamp(0.0, 1.0);
    let right = b[2].clamp(0.0, 1.0);
    let bottom = b[3].clamp(0.0, 1.0);
    if right - left < 0.02 || bottom - top < 0.02 {
        return Err("vision location is empty or too small".into());
    }
    let rect = [
        left * page_w,
        (1.0 - bottom) * page_h,
        right * page_w,
        (1.0 - top) * page_h,
    ];
    if let Some(caption) = caption_bbox {
        if rect_distance(rect, caption) > page_h * 0.3 {
            return Err("vision location is inconsistent with the indexed caption".into());
        }
    }
    Ok(rect)
}

fn rect_distance(a: [f32; 4], b: [f32; 4]) -> f32 {
    let dx = (a[0] - b[2]).max(b[0] - a[2]).max(0.0);
    let dy = (a[1] - b[3]).max(b[1] - a[3]).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

fn union_rect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
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
        "Render a region of a paper's PDF page to a PNG and return its path. For a named figure \
         or table, pass only `label` (for example \"图3\", \"Fig. 2\" or \"Table 1\"); the backend \
         computes the crop from PDF layout and uses the configured vision model only as a fallback. \
         Never guess `rect`: use it only when the user supplied the coordinates or a previous tool \
         result returned them. Use `region` only for an intentionally coarse top/middle/bottom/full \
         page capture. label, rect and region are mutually exclusive; omitting all three captures \
         the full page. For label captures, include_caption defaults to true. The result includes \
         the actual rect, page dimensions, crop_source and confidence. After calling this tool you \
         MUST embed the returned path as ![](path). A path alone does NOT let you see the image: \
         set analyze=true when you need to reason about its visual content. For tables, prefer a \
         markdown text table and attach the image when layout matters."
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
                description: "1-based page number. Optional with label; otherwise defaults to 1".into(),
                required: false,
            },
            ToolParameter {
                name: "label".into(),
                param_type: "string".into(),
                description: "Caption label of the figure/table to capture. Preferred for named figures and tables; do not combine with rect or region".into(),
                required: false,
            },
            ToolParameter {
                name: "rect".into(),
                param_type: "string".into(),
                description: "Explicit crop \"x0,y0,x1,y1\" in PDF points (y-up, origin bottom-left). Only use coordinates supplied by the user or returned by a tool; never estimate them".into(),
                required: false,
            },
            ToolParameter {
                name: "region".into(),
                param_type: "string".into(),
                description: "Coarse page region: top | middle | bottom | full. Do not combine with label or rect".into(),
                required: false,
            },
            ToolParameter {
                name: "include_caption".into(),
                param_type: "boolean".into(),
                description: "Include the caption in a label-driven crop (default true)".into(),
                required: false,
            },
            ToolParameter {
                name: "analyze".into(),
                param_type: "boolean".into(),
                description: "Analyze the captured PNG with the configured vision model (default false). Required when the agent itself needs visual evidence from the image".into(),
                required: false,
            },
            ToolParameter {
                name: "analyze_prompt".into(),
                param_type: "string".into(),
                description: "Custom instruction for content analysis (used only with analyze=true); visibility and evidence constraints still apply".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let paper_id = args["paper_id"].as_str().ok_or("paper_id required")?;
        let page_arg = args["page"].as_i64().map(|p| p.max(1) as u16);
        let label = args["label"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let rect_arg = args["rect"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let region_arg = args["region"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        validate_selectors(label, rect_arg, region_arg)?;
        let region = region_arg.unwrap_or("full");
        let include_caption = args["include_caption"].as_bool().unwrap_or(true);
        let analyze = args["analyze"].as_bool().unwrap_or(false);

        let paper: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT title, file_path FROM papers WHERE id = ?")
                .bind(paper_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| format!("db error: {e}"))?;
        let (title, file_path) = paper.ok_or_else(|| format!("paper not found: {paper_id}"))?;
        let rel_path = file_path.ok_or("该文献没有关联的 PDF 文件")?;
        let pdf_path = crate::file_store::resolve_blob_path(&self.app_data_dir, &rel_path);
        if !pdf_path.exists() {
            return Err(format!("PDF 文件不存在: {}", pdf_path.display()));
        }

        let mut matched_figure: Option<FigureRow> = None;
        let mut crop_source = "region";
        let mut confidence = 0.35;
        let mut location_note: Option<String> = None;

        let (page, mut rect, page_w, page_h): (u16, [f32; 4], f32, f32) = if let Some(lbl) = label {
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
                     You can still snapshot by page + region/rect, but do not guess rect coordinates.)",
                    page_arg.map(|p| format!(" on page {p}")).unwrap_or_default(),
                    if available.is_empty() { "(none)".into() } else { available.join(", ") },
                ));
            };

            let page = fig.page as u16;
            let (pw, ph) =
                figures::page_size(&pdf_path, page - 1).map_err(|e| format!("page size: {e}"))?;
            let mut fallback_rect = if let Some(b) = fig.bbox {
                let area = ((b[2] - b[0]) * (b[3] - b[1])).max(0.0);
                if area > pw * ph * 0.9 {
                    crop_source = "caption_inferred";
                    confidence = 0.2;
                } else {
                    crop_source = "indexed_image";
                    confidence = 0.8;
                }
                b
            } else if let Some(cb) = fig.caption_bbox {
                crop_source = "caption_inferred";
                confidence = 0.35;
                figures::infer_rect_from_caption(fig.kind, cb, pw, ph)
            } else {
                crop_source = "caption_inferred";
                confidence = 0.2;
                figures::region_rect("full", pw, ph).unwrap()
            };
            if include_caption {
                if let Some(cb) = fig.caption_bbox {
                    fallback_rect = union_rect(fallback_rect, cb);
                }
            }

            let layout_result = tokio::task::spawn_blocking({
                let pdf_path = pdf_path.clone();
                let kind = fig.kind;
                let caption_bbox = fig.caption_bbox;
                let indexed_bbox = fig.bbox;
                move || {
                    caption_bbox.map(|cb| {
                        crate::pdf::figure_layout::locate_figure_region(
                            &pdf_path,
                            page - 1,
                            kind,
                            cb,
                            indexed_bbox,
                            include_caption,
                        )
                    })
                }
            })
            .await
            .map_err(|e| format!("layout task: {e}"))?;

            let mut rect = fallback_rect;
            match layout_result {
                Some(Ok(Some(located))) => {
                    rect = located.rect;
                    crop_source = located.source;
                    confidence = located.confidence;
                }
                Some(Ok(None)) => {
                    crop_source = "caption_inferred";
                    confidence = confidence.min(0.35);
                    location_note = Some(
                        "no reliable layout boundary was found; the fallback crop requires verification"
                            .to_string(),
                    );
                }
                Some(Err(e)) => {
                    location_note = Some(format!("layout location failed, using fallback: {e}"));
                }
                _ => {}
            }

            if confidence < VISION_REFINE_BELOW && self.vision_llm.is_some() {
                match self
                    .locate_with_vision(
                        &pdf_path,
                        page,
                        pw,
                        ph,
                        &fig.label,
                        &fig.caption,
                        fig.caption_bbox,
                        include_caption,
                    )
                    .await
                {
                    Ok((vision_rect, vision_confidence)) => {
                        rect = vision_rect;
                        crop_source = "vision";
                        confidence = vision_confidence;
                        location_note = None;
                    }
                    Err(e) => {
                        location_note = Some(format!(
                            "vision refinement unavailable; using layout crop: {e}"
                        ));
                    }
                }
            }
            matched_figure = Some(fig);
            (page, rect, pw, ph)
        } else if let Some(r) = rect_arg {
            let page = page_arg.unwrap_or(1);
            let rect = figures::parse_rect(r).ok_or_else(|| {
                format!("invalid rect \"{r}\" — expected \"x0,y0,x1,y1\" in PDF points")
            })?;
            let (pw, ph) =
                figures::page_size(&pdf_path, page - 1).map_err(|e| format!("page size: {e}"))?;
            crop_source = "explicit_rect";
            confidence = 1.0;
            (page, rect, pw, ph)
        } else {
            let page = page_arg.unwrap_or(1);
            let (pw, ph) =
                figures::page_size(&pdf_path, page - 1).map_err(|e| format!("page size: {e}"))?;
            let rect = figures::region_rect(region, pw, ph).ok_or_else(|| {
                format!("invalid region \"{region}\" — expected top | middle | bottom | full")
            })?;
            (page, rect, pw, ph)
        };

        if label.is_some() {
            rect = figures::pad_rect(rect, BBOX_PAD, page_w, page_h);
        }
        rect = figures::pad_rect(rect, 0.0, page_w, page_h);

        let dir = self.app_data_dir.join("snapshots");
        let hint = if label.is_some() {
            label.unwrap_or("snap")
        } else if rect_arg.is_some() {
            "rect"
        } else {
            region
        };
        let name = snapshot_filename(paper_id, page, hint, rect);
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

        if let Some(fig) = &matched_figure {
            let _ = sqlx::query("UPDATE paper_figures SET image_path = ? WHERE id = ?")
                .bind(out_path.to_string_lossy().to_string())
                .bind(&fig.id)
                .execute(&self.db)
                .await;
        }

        let shown_path = display_path(&out_path);
        let mut info = serde_json::json!({
            "path": shown_path,
            "paper": title,
            "paper_id": paper_id,
            "page": page,
            "width": width,
            "height": height,
            "rect": rect,
            "page_width": page_w,
            "page_height": page_h,
            "crop_source": crop_source,
            "confidence": confidence_label(confidence),
            "confidence_score": (confidence * 100.0).round() / 100.0,
            "includes_caption": label.is_some() && include_caption,
        });
        if let Some(fig) = &matched_figure {
            info["label"] = serde_json::json!(fig.label);
            info["kind"] = serde_json::json!(fig.kind.as_str());
            if !fig.caption.is_empty() {
                info["caption"] =
                    serde_json::json!(fig.caption.chars().take(200).collect::<String>());
            }
        }
        let mut notes = Vec::new();
        if confidence < 0.6 || crop_source == "region" {
            notes.push("The crop is approximate or intentionally coarse. Describe only visible content and verify conclusions with paper_read.".to_string());
        }
        if let Some(note) = location_note {
            notes.push(note);
        }
        if !notes.is_empty() {
            info["note"] = serde_json::json!(notes.join("\n"));
        }

        let mut result = format!(
            "{info}\n\n\
             截图已保存。**在回复正文中用 ![]({}) 嵌入此图**，用户即可在气泡中查看论文原图。\
             crop_source/confidence 表示裁剪边界可靠性；低置信度或内容不完整时不得作确定结论，须用 paper_read 核验。\
             analyze=false 时你并未看到图片内容，不得把图片当作已分析证据。表格内容优先整理为 markdown 文本表格。",
            shown_path
        );

        if analyze {
            result.push_str(
                &self
                    .analyze_snapshot(
                        &out_path,
                        label,
                        args["analyze_prompt"].as_str(),
                        crop_source,
                        confidence,
                        label.is_some() && include_caption,
                    )
                    .await,
            );
        }
        Ok(result)
    }
}

impl PaperSnapshotTool {
    async fn locate_with_vision(
        &self,
        pdf_path: &std::path::Path,
        page: u16,
        page_w: f32,
        page_h: f32,
        label: &str,
        caption: &str,
        caption_bbox: Option<[f32; 4]>,
        include_caption: bool,
    ) -> Result<([f32; 4], f32), String> {
        let cfg = self
            .vision_llm
            .as_ref()
            .ok_or("no vision model configured")?;
        let temp_path = std::env::temp_dir().join(format!(
            "siku-snapshot-location-{}.png",
            uuid::Uuid::new_v4()
        ));
        let full_page = figures::region_rect("full", page_w, page_h).unwrap();
        tokio::task::spawn_blocking({
            let pdf_path = pdf_path.to_path_buf();
            let temp_path = temp_path.clone();
            move || {
                crate::pdf::renderer::render_page_region(
                    &pdf_path,
                    page - 1,
                    full_page,
                    LOCATION_DPI,
                    &temp_path,
                )
            }
        })
        .await
        .map_err(|e| format!("vision page render task: {e}"))?
        .map_err(|e| format!("vision page render failed: {e}"))?;

        let bytes = tokio::fs::read(&temp_path)
            .await
            .map_err(|e| format!("read vision page: {e}"))?;
        let _ = tokio::fs::remove_file(&temp_path).await;
        let image = ImagePart {
            mime: "image/png".to_string(),
            base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
        };
        let caption_hint = caption.chars().take(300).collect::<String>();
        let prompt = format!(
            "Locate the complete academic figure or table labelled \"{label}\" on this full PDF page. \
             Caption context: \"{caption_hint}\". Return ONLY one JSON object with this shape: \
             {{\"bbox\":[left,top,right,bottom],\"confidence\":0.0}}. Coordinates are normalized \
             to 0..1 relative to the whole page image; top is the upper edge. Include all subplots, \
             axes, tick labels, legends, table rules and cells. {} Exclude body paragraphs, unrelated \
             figures/tables and large empty margins. Do not explain anything.",
            if include_caption {
                "Include the printed caption."
            } else {
                "Exclude the printed caption."
            }
        );
        let client =
            llm::client::create_llm_client(cfg).map_err(|e| format!("vision client: {e}"))?;
        let response = client
            .chat_completion_vision(
                "You precisely locate figures and tables in academic PDF pages and output only valid JSON.",
                &prompt,
                &[image],
            )
            .await
            .map_err(|e| format!("vision location request failed: {e}"))?;
        let (location, confidence) = parse_vision_location(&response.content)?;
        let rect = normalized_vision_rect(&location, page_w, page_h, caption_bbox)?;
        Ok((rect, confidence))
    }

    async fn analyze_snapshot(
        &self,
        png_path: &std::path::Path,
        label: Option<&str>,
        custom_prompt: Option<&str>,
        crop_source: &str,
        confidence: f32,
        includes_caption: bool,
    ) -> String {
        let Some(cfg) = &self.vision_llm else {
            return "\n\n[图像理解] 未为本智能体配置多模态（视觉）模型，已跳过 analyze；\
                    图片本身仍可正常展示，但你不能声称已分析图片内容。可在会话设置中选择视觉模型后重试。"
                .to_string();
        };
        let bytes = match std::fs::read(png_path) {
            Ok(b) => b,
            Err(e) => return format!("\n\n[图像理解] 读取截图失败: {e}"),
        };
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        let content_prompt = custom_prompt.unwrap_or(
            "这是学术论文中的一张图或表。请描述其内容与结论：若是图，说明图的类型、坐标轴含义、\
             主要数据趋势与结论；若是表，请以 markdown 表格转录其中实际可见的数据。用中文回答。",
        );
        let label_context = label
            .map(|l| format!("该截图对应论文中的「{l}」。"))
            .unwrap_or_default();
        let prompt = format!(
            "{label_context}\n裁剪来源：{crop_source}；边界置信度：{}（{confidence:.2}）；包含图注：{includes_caption}。\n\
             {content_prompt}\n\n证据约束：仅依据图片中实际可见的内容，不得按常见论文结构、其他正文或常识补全。\
             若坐标轴、图例、数据区域、表格行列或其他关键部分被裁缺，必须明确指出信息不足；\
             低置信度裁剪只能作为待核验证据。表格不得臆造不可见的行、列或数值。",
            confidence_label(confidence),
        );
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
                "You are an expert at reading academic figures and tables. Never infer content that is not visible.",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_are_mutually_exclusive() {
        assert!(validate_selectors(Some("Fig. 1"), Some("1,2,3,4"), None).is_err());
        assert!(validate_selectors(None, Some("1,2,3,4"), Some("full")).is_err());
        assert!(validate_selectors(Some("Fig. 1"), None, None).is_ok());
        assert!(validate_selectors(None, None, None).is_ok());
    }

    #[test]
    fn snapshot_names_are_stable_and_distinct_for_different_rects() {
        let a = snapshot_filename("12345678-abcd", 3, "rect", [10.0, 20.0, 100.0, 200.0]);
        let same = snapshot_filename("12345678-abcd", 3, "rect", [10.0, 20.0, 100.0, 200.0]);
        let b = snapshot_filename("12345678-abcd", 3, "rect", [10.0, 20.0, 120.0, 200.0]);
        assert_eq!(a, same);
        assert_ne!(a, b);
    }

    #[test]
    fn parses_and_converts_vision_bbox() {
        let raw = "```json\n{\"bbox\":[0.1,0.2,0.8,0.7],\"confidence\":0.91}\n```";
        let (location, confidence) = parse_vision_location(raw).unwrap();
        assert!((confidence - 0.91).abs() < 0.001);
        let rect = normalized_vision_rect(&location, 600.0, 800.0, None).unwrap();
        for (actual, expected) in rect.into_iter().zip([60.0, 240.0, 480.0, 640.0]) {
            assert!((actual - expected).abs() < 0.001);
        }
    }

    #[test]
    fn rejects_invalid_or_low_confidence_vision_locations() {
        assert!(parse_vision_location("no json").is_err());
        assert!(parse_vision_location("{\"bbox\":[0,0,1,1],\"confidence\":0.2}").is_err());
        let (location, _) = parse_vision_location("{\"bbox\":[0.1,0.1,0.1,0.8]}").unwrap();
        assert!(normalized_vision_rect(&location, 600.0, 800.0, None).is_err());
    }

    #[tokio::test]
    async fn label_snapshot_end_to_end_returns_crop_metadata() {
        let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../demo2.pdf");
        if !source.exists() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(&source, dir.path().join("paper.pdf")).unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE papers (id TEXT PRIMARY KEY, title TEXT, file_path TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE paper_figures (
                id TEXT PRIMARY KEY, paper_id TEXT, page INTEGER, kind TEXT,
                label TEXT, caption TEXT, bbox TEXT, caption_bbox TEXT, image_path TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let paper_id = "12345678-snapshot-test";
        sqlx::query("INSERT INTO papers (id, title, file_path) VALUES (?, ?, ?)")
            .bind(paper_id)
            .bind("StrokeGuard")
            .bind("paper.pdf")
            .execute(&pool)
            .await
            .unwrap();
        let records = crate::pdf::figures::extract_figures(&source).unwrap();
        let fig = records
            .iter()
            .find(|r| r.page == 1 && r.label == "Fig. 1")
            .expect("demo2 Fig. 1");
        let bbox = fig.bbox.map(|b| serde_json::to_string(&b).unwrap());
        let caption_bbox = fig.caption_bbox.map(|b| serde_json::to_string(&b).unwrap());
        sqlx::query(
            "INSERT INTO paper_figures (
                id, paper_id, page, kind, label, caption, bbox, caption_bbox
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("figure-1")
        .bind(paper_id)
        .bind(fig.page as i32)
        .bind(fig.kind.as_str())
        .bind(&fig.label)
        .bind(&fig.caption)
        .bind(bbox)
        .bind(caption_bbox)
        .execute(&pool)
        .await
        .unwrap();

        let tool = PaperSnapshotTool::new(pool, dir.path().to_path_buf(), None);
        let output = tool
            .execute(serde_json::json!({"paper_id": paper_id, "label": "Fig. 1"}))
            .await
            .unwrap();
        let info: serde_json::Value = serde_json::from_str(output.lines().next().unwrap()).unwrap();
        assert_eq!(info["crop_source"], "image_layout");
        assert_eq!(info["confidence"], "high");
        assert_eq!(info["includes_caption"], true);
        assert_eq!(info["rect"].as_array().unwrap().len(), 4);
        assert!(info["rect"][0].as_f64().unwrap() > 0.0);
        assert!(info["rect"][3].as_f64().unwrap() < info["page_height"].as_f64().unwrap());
        let path = info["path"].as_str().unwrap();
        assert!(std::path::Path::new(path).exists());
        assert!(output.contains("analyze=false"));
    }
}
