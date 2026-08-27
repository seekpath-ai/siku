use async_trait::async_trait;
use base64::Engine;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::ai::llm::{self, ImagePart};
use super::path::{resolve_path, working_dir_from_args};

fn guess_mime(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

/// Read an image and analyze it with the agent's vision (multimodal) model.
pub struct ReadMediaFileTool {
    vision_llm: Option<llm::LlmConfig>,
}

/// Maximum image size accepted by read_media_file (20 MB).
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

impl ReadMediaFileTool {
    pub fn new(vision_llm: Option<llm::LlmConfig>) -> Self {
        Self { vision_llm }
    }
}

#[async_trait]
impl Tool for ReadMediaFileTool {
    fn name(&self) -> &str {
        "read_media_file"
    }

    fn description(&self) -> &str {
        "Read an image file from disk and analyze it with the agent's vision (multimodal) model, returning a text description. Requires a vision-capable model configured for this agent. Use this ONLY for image files on disk referenced by a path; images attached directly in the conversation are already visible to you — never call this tool for those."
    }

    fn readonly(&self) -> bool {
        true
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![
            ToolParameter {
                name: "path".into(),
                param_type: "string".into(),
                description: "Image file path (absolute, or relative to the working directory)".into(),
                required: true,
            },
            ToolParameter {
                name: "prompt".into(),
                param_type: "string".into(),
                description: "Optional instruction for analyzing the image (default: describe it in detail)".into(),
                required: false,
            },
        ]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("path required")?;
        let prompt = args["prompt"].as_str().unwrap_or(
            "Describe this image in detail, including any text, diagrams, tables, or notable elements.",
        );
        let wd = working_dir_from_args(&args);
        let resolved = resolve_path(wd.as_deref(), path)?;

        if !resolved.is_file() {
            return Ok(format!("Not a file: {path}"));
        }
        // Refuse oversized files before reading them into memory + base64.
        if let Ok(meta) = std::fs::metadata(&resolved) {
            if meta.len() > MAX_FILE_BYTES {
                return Err(format!(
                    "file too large: {} bytes (max {} MB); compress or downscale the image first",
                    meta.len(),
                    MAX_FILE_BYTES / 1024 / 1024
                ));
            }
        }
        let bytes = std::fs::read(&resolved).map_err(|e| format!("read failed: {e}"))?;
        let mime = guess_mime(&resolved.to_string_lossy());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let Some(cfg) = &self.vision_llm else {
            return Ok(format!(
                "Image at {path} ({mime}, {} bytes). No vision model is configured for this agent, so it cannot be analyzed.",
                bytes.len()
            ));
        };

        let client = llm::client::create_llm_client(cfg)
            .map_err(|e| format!("vision client: {e}"))?;
        let image = ImagePart {
            mime: mime.to_string(),
            base64: b64,
        };
        let resp = client
            .chat_completion_vision("You are an expert image analyst.", prompt, &[image])
            .await
            .map_err(|e| format!("vision request failed: {e}"))?;

        Ok(format!("[Image: {path}]\n{}", resp.content))
    }
}
