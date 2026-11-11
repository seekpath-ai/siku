use async_trait::async_trait;
use crate::ai::agent::tool_registry::{Tool, ToolParameter};

/// 安装技能隔离 Python 环境（Miniconda + siku-office env + pip 依赖）。
/// 仅当挂载的技能声明了 pip 依赖时注册；SKILL.md 的运行环境块引导模型在
/// 环境未就绪时调用它。
pub struct SkillEnvSetupTool {
    app_data_dir: std::path::PathBuf,
    /// 挂载技能声明的 pip 依赖并集（安装时一次装齐）。
    packages: Vec<String>,
}

impl SkillEnvSetupTool {
    pub fn new(app_data_dir: std::path::PathBuf, packages: Vec<String>) -> Self {
        Self {
            app_data_dir,
            packages,
        }
    }
}

#[async_trait]
impl Tool for SkillEnvSetupTool {
    fn name(&self) -> &str {
        "skill_env_setup"
    }

    fn description(&self) -> &str {
        "安装技能所需的隔离 Python 环境（Miniconda + 依赖包，不改动系统 Python）。\
         仅在技能提示运行环境未就绪时调用；下载加安装可能耗时数分钟，属正常现象。"
    }

    fn parameters(&self) -> Vec<ToolParameter> {
        vec![]
    }

    /// Miniconda 下载（~120MB）+ conda create + pip install 在慢网络下可能
    /// 超过默认的 320s 工具超时；给足 30 分钟。
    fn timeout_secs(&self) -> u64 {
        1800
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let python = crate::core::skill_env::ensure_env(&self.app_data_dir, &self.packages, None)
            .await?;
        Ok(format!(
            "隔离环境已就绪。之后本技能的脚本一律用这个解释器运行（不要用系统 python3）：\n{}",
            python.display()
        ))
    }
}
