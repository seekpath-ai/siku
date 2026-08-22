use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::instrument;

use crate::ai::llm::{self, ChatMessage};

// ── Region type enum ──

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum RegionType {
    Title,
    Authors,
    Abstract,
    Body,
    Heading,
    Figure,
    Table,
    Equation,
    References,
    #[serde(alias = "unknown")]
    Unknown,
}

impl RegionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Authors => "authors",
            Self::Abstract => "abstract",
            Self::Body => "body",
            Self::Heading => "heading",
            Self::Figure => "figure",
            Self::Table => "table",
            Self::Equation => "equation",
            Self::References => "references",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for RegionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ── Input types (from frontend) ──

/// A single text item from pdf.js with position and font metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextItemInput {
    pub str: String,
    /// X offset in px from page left.
    pub x: f64,
    /// Y offset in px from page top (already flipped from PDF coords).
    pub y: f64,
    /// Font size in px.
    #[serde(rename = "fontSize")]
    pub font_size: f64,
    /// Font name from pdf.js.
    #[serde(rename = "fontName")]
    pub font_name: String,
    /// Advance width of the text string in px (from pdf.js).
    pub width: f64,
}

/// Detection request for a single page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionDetectionRequest {
    pub page: u32,
    #[serde(rename = "pageWidth")]
    pub page_width: f64,
    #[serde(rename = "pageHeight")]
    pub page_height: f64,
    #[serde(rename = "totalPages")]
    pub total_pages: u32,
    pub items: Vec<TextItemInput>,
}

// ── Output types (returned to frontend) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedRegionOutput {
    pub id: String,
    #[serde(rename = "type")]
    pub region_type: RegionType,
    #[serde(rename = "pageIndex")]
    pub page_index: u32,
    #[serde(rename = "yRatio")]
    pub y_ratio: f64,
    #[serde(rename = "xRatio")]
    pub x_ratio: f64,
    #[serde(rename = "heightRatio")]
    pub height_ratio: f64,
    #[serde(rename = "widthRatio")]
    pub width_ratio: f64,
    pub text: Option<String>,
    pub confidence: f32,
}

const SYSTEM_PROMPT: &str = "\
Output a JSON array of structural regions found on this academic paper page. \
Do NOT write any analysis, reasoning, or explanation. Output ONLY the JSON array, \
starting with [ and ending with ].

Region types: title, authors, abstract, heading, body, figure, table, equation, references.

Rules:
- title: largest font, page 1, top 30%
- authors: after title, medium font, short lines with commas/emails
- abstract: after authors, starts with \"Abstract\" or body text following abstract label
- heading: font between title and body, 1-2 short lines, often numbered
- body: most common font size, long paragraphs
- figure: caption starting with \"Fig\" / \"Figure\"
- table: caption starting with \"Table\"
- equation: centered, math font names (CMSY/CMMI/MSAM) or math symbols
- references: starts with \"References\"/\"Bibliography\", small font, [1]-style entries

Output format (include \"page\" field for multi-page responses):
[{\"page\":1,\"type\":\"title\",\"yRatio\":0.09,\"xRatio\":0.15,\"heightRatio\":0.04,\"widthRatio\":0.7,\"confidence\":0.95,\"text\":\"Paper Title\"}]

Coordinates are 0.0-1.0 fractions of page dimensions. Include \"text\" field with the combined text.\n\
\n\
IMPORTANT coordinate convention: The y value in each LINE entry is the text BASELINE, NOT the visual top. The \"font\" value is the font size in px. To compute region boundaries: visual top = y - font (ascenders extend ~1×font above baseline); visual bottom = y (baseline, descenders are negligible). So yRatio = (y - font) / pageHeight, and heightRatio = (bottom_y - top_y) / pageHeight. For multi-line regions, use the visual top of the first line and the baseline of the last line.";

/// Build the user prompt describing the page layout.
fn build_layout_prompt(request: &RegionDetectionRequest) -> String {
    // Cluster items into lines by Y proximity, sort top→bottom, left→right within line
    let mut items = request.items.clone();
    items.sort_by(|a, b| {
        a.y.partial_cmp(&b.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Cluster into lines (items within 3px Y tolerance are same line)
    let mut lines: Vec<Vec<&TextItemInput>> = Vec::new();
    for item in &items {
        let mut placed = false;
        for line in &mut lines {
            if (line[0].y - item.y).abs() <= 3.0 {
                line.push(item);
                // Re-sort by X within the line
                line.sort_by(|a, b| {
                    a.x.partial_cmp(&b.x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                placed = true;
                break;
            }
        }
        if !placed {
            lines.push(vec![item]);
        }
    }

    // Sort lines top→bottom
    lines.sort_by(|a, b| {
        a[0].y
            .partial_cmp(&b[0].y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Build a compact text representation
    let mut desc = format!(
        "PAGE {} of {} (width={:.0}, height={:.0})\n",
        request.page, request.total_pages, request.page_width, request.page_height
    );

    for line in &lines {
        let y = line[0].y;
        let x_start = line.first().map(|i| i.x).unwrap_or(0.0);
        let x_end = line.last().map(|i| {
    if i.width > 0.0 { i.x + i.width } else { i.x + (i.str.len() as f64 * i.font_size * 0.5) }
}).unwrap_or(0.0);
        let font_size = line.iter().map(|i| i.font_size).fold(0.0f64, f64::max);
        let text: String = line.iter().map(|i| i.str.as_str()).collect::<Vec<_>>().join(" ");

        // Truncate very long lines to keep prompt compact
        let line_limit = crate::core::settings_service::cached_settings()
            .region_detection_line_max_chars
            .max(1) as usize;
        let text_short = if text.chars().count() > line_limit {
            format!("{}...", text.chars().take(line_limit).collect::<String>())
        } else {
            text
        };

        desc.push_str(&format!(
            "LINE y={y:.0} x={x_start:.0}-{x_end:.0} font={font_size:.0}: \"{text_short}\"\n"
        ));
    }

    desc
}

/// Parse the LLM JSON response into region outputs.
fn parse_response(raw: &str, page_index: u32) -> Result<Vec<DetectedRegionOutput>, String> {
    // The LLM might wrap JSON in ```json fences or include trailing text
    let json_str = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Try to find the JSON array
    let start = json_str.find('[').ok_or("no JSON array found in response")?;
    let end = json_str.rfind(']').ok_or("no JSON array end found in response")?;
    let array_str = &json_str[start..=end];

    #[derive(Deserialize)]
    struct RawRegion {
        #[serde(rename = "type")]
        region_type: RegionType,
        #[serde(rename = "yRatio")]
        y_ratio: f64,
        #[serde(rename = "xRatio")]
        x_ratio: f64,
        #[serde(rename = "heightRatio")]
        height_ratio: f64,
        #[serde(rename = "widthRatio")]
        width_ratio: f64,
        confidence: Option<f32>,
        text: Option<String>,
    }

    let raw_regions: Vec<RawRegion> =
        serde_json::from_str(array_str).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let regions = raw_regions
        .into_iter()
        .map(|r| DetectedRegionOutput {
            id: format!("{}-p{}-{}", r.region_type, page_index, (r.y_ratio * 1000.0) as i32),
            region_type: r.region_type,
            page_index,
            y_ratio: r.y_ratio.clamp(0.0, 1.0),
            x_ratio: r.x_ratio.clamp(0.0, 1.0),
            height_ratio: r.height_ratio.clamp(0.0, 1.0),
            width_ratio: r.width_ratio.clamp(0.0, 1.0),
            text: r.text,
            confidence: r.confidence.unwrap_or(0.7),
        })
        .collect();

    Ok(regions)
}

/// Detect structural regions on a single PDF page using LLM.
#[instrument(skip(db))]
pub async fn detect_regions(
    db: &SqlitePool,
    request: RegionDetectionRequest,
) -> Result<Vec<DetectedRegionOutput>, String> {
    let mut llm_config = crate::core::settings_service::load_llm_config(db).await?;

    if llm_config.api_key.is_empty() && llm_config.provider != llm::LlmProvider::Ollama {
        return Err("API key not configured. Please set it in Settings.".to_string());
    }

    // Region detection needs more output tokens than default (page layout analysis).
    // Region detection needs the maximum reasonable token budget.
    // Reasoning models can burn thousands of tokens on chain-of-thought
    // before emitting JSON — never truncate the output.
    if llm_config.max_tokens < 16384 {
        llm_config.max_tokens = 16384;
    }
    // Lower temperature for deterministic JSON output
    llm_config.temperature = llm_config.temperature.min(0.3);

    let page_index = request.page;
    let layout_desc = build_layout_prompt(&request);

    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: SYSTEM_PROMPT.to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: layout_desc,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ];

    let client = llm::client::create_llm_client(&llm_config)
        .map_err(|e| format!("failed to create LLM client: {e}"))?;

    let resp = client
        .chat_completion(&messages, &[])
        .await
        .map_err(|e| format!("region detection failed: {e}"))?;

    let preview_limit = crate::core::settings_service::cached_settings()
        .log_region_detection_preview_max_chars
        .max(1) as usize;
    let preview = match resp.content.char_indices().nth(preview_limit) {
        Some((idx, _)) => &resp.content[..idx],
        None => &resp.content,
    };
    tracing::info!(
        page = page_index,
        tokens_in = resp.tokens_in,
        tokens_out = resp.tokens_out,
        response_preview = %preview,
        "LLM region detection complete"
    );

    let regions = parse_response(&resp.content, page_index)?;
    tracing::info!(
        page = page_index,
        count = regions.len(),
        types = ?regions.iter().map(|r| &r.region_type).collect::<Vec<_>>(),
        "parsed regions"
    );
    Ok(regions)
}
