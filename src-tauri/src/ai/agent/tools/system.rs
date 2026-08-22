use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

pub struct SystemInfoTool;

impl SystemInfoTool {
    pub fn new() -> Self { Self }
}

#[async_trait]
impl Tool for SystemInfoTool {
    fn name(&self) -> &str { "system_info" }

    fn readonly(&self) -> bool { true }

    fn description(&self) -> &str {
        "Get information about the user's system: OS, architecture, hostname, current time, and available resources."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        let now = crate::core::time::now_iso();

        let info = format!(
            "System Information:\n\
             - OS: {}\n\
             - Architecture: {}\n\
             - Hostname: {}\n\
             - Current time (UTC): {}\n\
             - CPU cores: {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH,
            hostname,
            now,
            cpu_count,
        );

        Ok(info)
    }
}
