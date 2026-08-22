use thiserror::Error;

#[derive(Error, Debug)]
pub enum SikuError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("PDF parsing error: {0}")]
    PdfParse(String),

    #[error("AI service error: {0}")]
    Ai(String),

    #[error("Agent error: {0}")]
    Agent(String),

    #[error("tool execution error: {0}")]
    ToolExecution(String),

    #[error("translation error: {0}")]
    Translation(String),

    #[error("research source error: {0}")]
    Research(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("rate limited")]
    RateLimited,

    #[error("API key not configured for provider {0}")]
    MissingApiKey(String),

    #[error("unknown error: {0}")]
    Unknown(String),
}

impl From<SikuError> for String {
    fn from(err: SikuError) -> Self {
        err.to_string()
    }
}

pub type Result<T> = std::result::Result<T, SikuError>;
