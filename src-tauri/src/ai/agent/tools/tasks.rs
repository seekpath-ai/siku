use std::sync::atomic::Ordering;
use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};
use crate::core::tasks::{snapshot, TaskStore};

const OUTPUT_PREVIEW_CHARS: usize = 32 * 1024;

/// List background tasks (read-only).
pub struct TaskListTool {
    tasks: TaskStore,
}

impl TaskListTool {
    pub fn new(tasks: TaskStore) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }

    fn description(&self) -> &str {
        "List background tasks started via bash with run_in_background=true, with their status and output log paths. Read-only, auto-approved."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let list = snapshot(&self.tasks).await;
        if list.is_empty() {
            return Ok("No background tasks.".to_string());
        }
        Ok(list
            .iter()
            .map(|t| {
                format!(
                    "- `{}` [{}] {} — exit: {:?} (log: {})",
                    t.id,
                    t.status,
                    t.description,
                    t.exit_code,
                    t.output_path.as_deref().unwrap_or(""),
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// Show a background task's output (read-only).
pub struct TaskOutputTool {
    tasks: TaskStore,
}

impl TaskOutputTool {
    pub fn new(tasks: TaskStore) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "task_output"
    }

    fn description(&self) -> &str {
        "Show the current output of a background task. Read-only, auto-approved."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "task_id".into(),
            param_type: "string".into(),
            description: "Background task id".into(),
            required: true,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let task_id = args["task_id"].as_str().ok_or("task_id required")?;
        let map = self.tasks.lock().await;
        let handle = map.get(task_id).ok_or_else(|| format!("task not found: {task_id}"))?;
        let info = &handle.info;
        let mut out = format!(
            "task `{}` [{}] exit: {:?}\n",
            info.id, info.status, info.exit_code
        );
        if let Some(path) = &info.output_path {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    let truncated = content.chars().count() > OUTPUT_PREVIEW_CHARS;
                    let preview: String = content
                        .chars()
                        .take(OUTPUT_PREVIEW_CHARS)
                        .collect();
                    out.push_str(&preview);
                    if truncated {
                        out.push_str(&format!(
                            "\n[...output truncated, full log: {path}]"
                        ));
                    }
                }
                Err(e) => out.push_str(&format!("\n(log read error: {e})")),
            }
        }
        Ok(out)
    }
}

/// Stop a running background task.
pub struct TaskStopTool {
    tasks: TaskStore,
}

impl TaskStopTool {
    pub fn new(tasks: TaskStore) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "task_stop"
    }

    fn description(&self) -> &str {
        "Stop a running background task. Requires approval."
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![ToolParameter {
            name: "task_id".into(),
            param_type: "string".into(),
            description: "Background task id".into(),
            required: true,
        }]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let task_id = args["task_id"].as_str().ok_or("task_id required")?;
        let map = self.tasks.lock().await;
        match map.get(task_id) {
            Some(handle) => {
                handle.cancel.store(true, Ordering::SeqCst);
                Ok(format!("Stopping task {task_id}"))
            }
            None => Err(format!("task not found: {task_id}")),
        }
    }
}
