//! 内置技能（pptx/docx/xlsx 等办公技能）的隔离 Python 运行环境。
//!
//! 原则：绝不触碰用户本机的 Python 环境（不往系统 python 里 pip install）。
//! 依赖装在 Miniconda 的独立环境 `siku-office` 里：
//! - 用户已有 conda（Miniconda/Anaconda/Miniforge）→ 在其中创建命名环境；
//! - 没有 conda → 应用下载 Miniconda 静默安装到应用数据目录（<app_data>/
//!   miniconda3，不注册系统 Python、不改 PATH），再在其中创建环境。
//!
//! 环境由办公三件套技能共享（python-pptx 现在用，后续 python-docx/openpyxl
//! 进同一环境），避免每个技能各背一个数百 MB 的环境。

use std::path::{Path, PathBuf};

/// 共享隔离环境名。
pub const ENV_NAME: &str = "siku-office";

/// 环境内固定的 Python 版本。
const ENV_PYTHON_VERSION: &str = "3.11";

/// 应用托管的 Miniconda 在应用数据目录下的文件夹名。
const MANAGED_DIR_NAME: &str = "miniconda3";

/// Miniconda 安装包地址（官方 + 清华镜像兜底）。
const MINICONDA_OFFICIAL: &str = "https://repo.anaconda.com/miniconda";
const MINICONDA_TUNA: &str = "https://mirrors.tuna.tsinghua.edu.cn/anaconda/miniconda";

/// 环境探测结果。两者都可能为 None（本机没有任何 conda）。
#[derive(Debug, Clone, Default)]
pub struct SkillEnv {
    /// 可用的 conda 可执行文件路径。
    pub conda: Option<PathBuf>,
    /// 隔离环境的 Python 解释器（环境已创建且存在时）。
    pub python: Option<PathBuf>,
    /// 探测时用的应用数据目录：技能工具在运行时按它重新探测（用户可能
    /// 刚在插件弹窗装完环境），快照只负责把这个根带过去。
    pub app_data_dir: PathBuf,
}

impl SkillEnv {
    pub fn ready(&self) -> bool {
        self.python.is_some()
    }
}

/// 应用托管 Miniconda 的安装根目录。
pub fn managed_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(MANAGED_DIR_NAME)
}

/// conda 可执行文件在 conda 根目录下的位置。
fn conda_exe_in(root: &Path) -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &["Scripts/conda.exe", "condabin/conda.bat"]
    } else {
        &["bin/conda", "condabin/conda"]
    };
    candidates
        .iter()
        .map(|rel| root.join(rel))
        .find(|p| p.is_file())
}

/// 隔离环境的 Python 解释器路径（存在才返回）。
fn env_python_in(root: &Path) -> Option<PathBuf> {
    let rel: &str = if cfg!(windows) {
        "python.exe"
    } else {
        "bin/python"
    };
    let p = root.join("envs").join(ENV_NAME).join(rel);
    p.is_file().then_some(p)
}

/// conda 根的候选列表，按优先级排序：
/// 1. 应用托管的 Miniconda（我们装的，环境也建在这里，路径确定）；
/// 2. CONDA_EXE 环境变量指向的安装；
/// 3. PATH 上的 conda；
/// 4. 各平台常见安装位置。
fn conda_roots(app_data_dir: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !roots.contains(&p) {
            roots.push(p);
        }
    };

    push(managed_root(app_data_dir));

    if let Some(exe) = std::env::var_os("CONDA_EXE") {
        // CONDA_EXE 形如 <root>/bin/conda 或 <root>\Scripts\conda.exe。
        let exe = PathBuf::from(exe);
        if let Some(root) = exe.parent().and_then(|p| p.parent()) {
            push(root.to_path_buf());
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            // PATH 上是 <root>/bin、<root>/condabin 或 <root>\Scripts。
            let names: &[&str] = if cfg!(windows) {
                &["conda.exe", "conda.bat"]
            } else {
                &["conda"]
            };
            if names.iter().any(|n| dir.join(n).is_file()) {
                if let Some(root) = dir.parent() {
                    push(root.to_path_buf());
                }
            }
        }
    }

    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from);
    if let Some(home) = home {
        for name in ["miniconda3", "anaconda3", "miniforge3"] {
            push(home.join(name));
        }
        if cfg!(windows) {
            if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
                for name in ["miniconda3", "anaconda3"] {
                    push(local.join(name));
                }
            }
            push(PathBuf::from(r"C:\ProgramData\miniconda3"));
            push(PathBuf::from(r"C:\ProgramData\anaconda3"));
        } else if cfg!(target_os = "macos") {
            push(PathBuf::from("/opt/miniconda3"));
            push(PathBuf::from("/opt/homebrew/Caskroom/miniconda/base"));
        } else {
            push(PathBuf::from("/opt/miniconda3"));
            push(PathBuf::from("/usr/local/miniconda3"));
        }
        // conda 根不可写时 `conda create -n` 会把环境建到 ~/.conda/envs ——
        // 它自身不是完整 conda 安装（没有 bin/conda），排在最后兜底。
        push(home.join(".conda"));
    }
    roots
}

/// 探测本机的技能运行环境。只做文件系统检查（conda 启动要 1-2s，每个候选
/// 都跑一遍太贵），调用方可以高频使用。
pub fn resolve(app_data_dir: &Path) -> SkillEnv {
    resolve_with_roots(app_data_dir, &conda_roots(app_data_dir))
}

/// resolve 的纯函数内核：候选根由调用方给出（测试与宿主环境隔离）。
fn resolve_with_roots(app_data_dir: &Path, roots: &[PathBuf]) -> SkillEnv {
    let base = || SkillEnv {
        conda: None,
        python: None,
        app_data_dir: app_data_dir.to_path_buf(),
    };
    // 已建好环境的 root 优先（环境在哪就用哪，不重复建）。conda 可执行文件
    // 可能不在同一个 root（~/.conda 只有 envs），回退到任一可用 conda。
    for root in roots {
        if let Some(python) = env_python_in(root) {
            let conda = conda_exe_in(root).or_else(|| roots.iter().find_map(|r| conda_exe_in(r)));
            return SkillEnv {
                conda,
                python: Some(python),
                ..base()
            };
        }
    }
    // 有 conda 但没有环境。
    if let Some(conda) = roots.iter().find_map(|r| conda_exe_in(r)) {
        return SkillEnv {
            conda: Some(conda),
            python: None,
            ..base()
        };
    }
    base()
}

/// 在指定解释器里试 import 一个模块（探测 pip 依赖是否就绪）。
pub fn python_module_available_in(python: &Path, module: &str) -> bool {
    crate::core::process::no_window(&mut std::process::Command::new(python))
        .args(["-c", &format!("import {module}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 注入给模型的「运行环境」说明块（技能前言声明了 pip 依赖时才需要）。
/// 三种状态：环境就绪（给解释器绝对路径）/ 有 conda 无环境 / 无 conda ——
/// 后两种都引导模型调用 `skill_env_setup` 工具完成安装。
pub fn prompt_block(env: &SkillEnv) -> String {
    if let Some(python) = &env.python {
        return format!(
            "## 运行环境（已就绪）\n\n\
             本技能的脚本必须用隔离环境 `{ENV_NAME}` 的 Python 解释器运行：\n\n\
             `{}`\n\n\
             依赖（python-pptx、pypdfium2 等）已装在其中。**禁止**使用系统 python3/python，\
             也**禁止**用 pip 往其它环境安装包——那会污染用户的 Python 环境。\
             若运行脚本报 ModuleNotFoundError，说明环境缺包，调用一次 `skill_env_setup` 补装后重试。",
            python.display()
        );
    }
    let mut block = String::from(
        "## 运行环境（未就绪）\n\n\
         本技能的脚本运行在隔离的 Miniconda 环境（`siku-office`）中，目前尚未安装。\
         **禁止**改用系统 python3/python 顶替，也**禁止**用 pip 往系统环境安装包。\n\n\
         请调用 `skill_env_setup` 工具完成安装",
    );
    if env.conda.is_some() {
        block.push_str("（检测到本机已有 conda，只会新建独立环境，约需 1-3 分钟）");
    } else {
        block.push_str(
            "（本机未检测到 conda，工具会先下载安装 Miniconda 到应用数据目录——\
             约 120MB 下载，不修改系统 PATH、不注册系统 Python——再创建环境，整体约需 3-10 分钟）",
        );
    }
    block.push_str(
        "。\n\n安装是全自动的；若工具调用被用户拒绝或失败，告知用户也可以在\
         「插件 → 技能详情 → 运行依赖」里点「安装隔离环境依赖」手动完成。",
    );
    block
}

// ── 安装编排 ─────────────────────────────────────────────────────────────

/// 安装过程的全局互斥：对话里的 skill_env_setup 与插件弹窗的按钮可能同时
/// 触发，conda 本身也不支持并发事务。
fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 安装进度回调（对话框用来刷新进度行；agent 工具路径传 None）。
pub type ProgressFn = Box<dyn Fn(&str) + Send + Sync>;

fn report(progress: &Option<&ProgressFn>, msg: &str) {
    if let Some(cb) = progress {
        cb(msg);
    }
    tracing::info!(target: "skill_env", "{msg}");
}

/// 当前平台的 Miniconda 安装包文件名。
fn miniconda_installer_name() -> Result<&'static str, String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("windows", "x86_64") => Ok("Miniconda3-latest-Windows-x86_64.exe"),
        ("macos", "aarch64") => Ok("Miniconda3-latest-MacOSX-arm64.sh"),
        ("macos", "x86_64") => Ok("Miniconda3-latest-MacOSX-x86_64.sh"),
        ("linux", "x86_64") => Ok("Miniconda3-latest-Linux-x86_64.sh"),
        ("linux", "aarch64") => Ok("Miniconda3-latest-Linux-aarch64.sh"),
        _ => Err(format!("暂不支持的平台：{os}/{arch}，请手动安装 Miniconda")),
    }
}

/// 下载 Miniconda 安装包（官方源失败时回退清华镜像）。
async fn download_miniconda(dest: &Path, progress: &Option<&ProgressFn>) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let name = miniconda_installer_name()?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建下载目录失败：{e}"))?;
    }
    let client = reqwest::Client::builder()
        .user_agent("siku-skill-env")
        .build()
        .map_err(|e| format!("创建下载客户端失败：{e}"))?;
    let mut last_err = String::new();
    for base in [MINICONDA_OFFICIAL, MINICONDA_TUNA] {
        let url = format!("{base}/{name}");
        report(progress, &format!("正在下载 Miniconda（{url}）…"));
        let resp = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                last_err = format!("HTTP {}", r.status());
                continue;
            }
            Err(e) => {
                last_err = e.to_string();
                continue;
            }
        };
        let total = resp.content_length().unwrap_or(0);
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|e| format!("创建安装包文件失败：{e}"))?;
        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        let mut downloaded: u64 = 0;
        let mut failed: Option<String> = None;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if let Err(e) = file.write_all(&bytes).await {
                        failed = Some(format!("写入安装包失败：{e}"));
                        break;
                    }
                    downloaded += bytes.len() as u64;
                    if total > 0 && downloaded % (16 * 1024 * 1024) < bytes.len() as u64 {
                        report(
                            progress,
                            &format!("正在下载 Miniconda… {}%", downloaded * 100 / total),
                        );
                    }
                }
                Err(e) => {
                    failed = Some(format!("下载中断：{e}"));
                    break;
                }
            }
        }
        if let Some(e) = failed {
            last_err = e;
            let _ = std::fs::remove_file(dest);
            continue;
        }
        // 安装包至少几十 MB；明显过小说明拿到的是错误页。
        if downloaded < 20 * 1024 * 1024 {
            last_err = format!("下载内容异常（仅 {} 字节）", downloaded);
            let _ = std::fs::remove_file(dest);
            continue;
        }
        return Ok(());
    }
    Err(format!(
        "Miniconda 下载失败（官方源与清华镜像均不可用）：{last_err}。\
         也可手动安装 Miniconda 后重试：https://docs.conda.io/projects/miniconda/en/latest/"
    ))
}

/// 同步运行一个命令并收集输出（用于 conda/pip，秒级到分钟级）。
fn run_capture(cmd: &mut std::process::Command, what: &str) -> Result<String, String> {
    let output = crate::core::process::no_window(cmd)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("启动{what}失败：{e}"))?;
    let mut log = String::from_utf8_lossy(&output.stdout).to_string();
    log.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        let tail: String = log.chars().rev().take(600).collect::<String>().chars().rev().collect();
        return Err(format!("{what}失败：\n{tail}"));
    }
    Ok(log)
}

/// 确保隔离环境可用：必要时下载安装托管 Miniconda、创建环境、装齐依赖。
/// 返回环境 Python 解释器路径。耗时数分钟，调用方须给足超时。
pub async fn ensure_env(
    app_data_dir: &Path,
    packages: &[String],
    progress: Option<&ProgressFn>,
) -> Result<PathBuf, String> {
    let _guard = install_lock().lock().await;

    // 环境已就绪：只补装缺失依赖（pip 对已满足的包秒回）。
    let env = resolve(app_data_dir);
    if let Some(python) = env.python.clone() {
        if !packages.is_empty() {
            pip_install(&python, packages, &progress).await?;
        }
        return Ok(python);
    }

    // 有 conda 无环境 → 直接建。
    let conda = match env.conda {
        Some(c) => c,
        None => {
            // 无 conda：下载托管 Miniconda 静默安装到应用数据目录。
            let root = managed_root(app_data_dir);
            let installer = app_data_dir
                .join("downloads")
                .join(miniconda_installer_name()?);
            download_miniconda(&installer, &progress).await?;
            report(&progress, "正在安装 Miniconda（应用目录内，不改动系统 Python）…");
            let root_clone = root.clone();
            let installer_clone = installer.clone();
            tokio::task::spawn_blocking(move || install_miniconda(&installer_clone, &root_clone))
                .await
                .map_err(|e| format!("安装任务失败：{e}"))??;
            let _ = std::fs::remove_file(&installer);
            conda_exe_in(&root).ok_or_else(|| {
                format!("Miniconda 安装后未找到 conda（{}），请重试", root.display())
            })?
        }
    };

    report(&progress, &format!("正在创建隔离环境 {ENV_NAME}（Python {ENV_PYTHON_VERSION}）…"));
    let conda_clone = conda.clone();
    tokio::task::spawn_blocking(move || {
        run_capture(
            std::process::Command::new(&conda_clone).args([
                "create",
                "-y",
                "-n",
                ENV_NAME,
                &format!("python={ENV_PYTHON_VERSION}"),
                "pip",
            ]),
            "创建隔离环境",
        )
    })
    .await
    .map_err(|e| format!("创建隔离环境任务失败：{e}"))??;

    let python = resolve(app_data_dir).python.ok_or_else(|| {
        "隔离环境创建后未找到其 Python 解释器（conda 可能把环境建到了非常规位置），请重试"
            .to_string()
    })?;
    if !packages.is_empty() {
        pip_install(&python, packages, &progress).await?;
    }
    report(&progress, "隔离环境就绪");
    Ok(python)
}

/// 用环境内的 pip 安装依赖；pypi 官方源失败时回退清华镜像。
async fn pip_install(python: &Path, packages: &[String], progress: &Option<&ProgressFn>) -> Result<(), String> {
    let python = python.to_path_buf();
    let packages: Vec<String> = packages.to_vec();
    let progress_line = format!("正在安装依赖包（{}）…", packages.join("、"));
    report(progress, &progress_line);
    tokio::task::spawn_blocking(move || {
        let base_args = |extra: &[&str]| {
            let mut cmd = std::process::Command::new(&python);
            cmd.args(["-m", "pip", "install"]).args(extra).args(&packages);
            cmd
        };
        match run_capture(&mut base_args(&[]), "安装依赖包") {
            Ok(log) => Ok(log),
            Err(e) => {
                // 网络原因回退清华镜像重试一次。
                run_capture(
                    &mut base_args(&["-i", "https://pypi.tuna.tsinghua.edu.cn/simple"]),
                    "安装依赖包（镜像重试）",
                )
                .map_err(|e2| format!("{e}\n镜像重试亦失败：{e2}"))
            }
        }
    })
    .await
    .map_err(|e| format!("安装依赖任务失败：{e}"))??;
    Ok(())
}

/// 静默安装 Miniconda 到指定目录。Unix 用 `-b -p`；Windows 用 NSIS 静默
/// 参数（JustMe、不写 PATH、不注册系统 Python——对本机零侵入）。
fn install_miniconda(installer: &Path, root: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        // /D 必须是最后一个参数且不加引号；NSIS 静默安装可能派生子进程后
        // 提前退出，装完以 conda.exe 出现为准（最多等 3 分钟）。
        let root_str = root.to_string_lossy().to_string();
        let status = crate::core::process::no_window(&mut std::process::Command::new(installer))
            .args([
                "/InstallationType=JustMe",
                "/RegisterPython=0",
                "/AddToPath=0",
                "/S",
            ])
            .arg(format!("/D={root_str}"))
            .stdin(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("启动 Miniconda 安装程序失败：{e}"))?;
        if !status.success() {
            return Err(format!("Miniconda 安装程序退出码异常：{status}"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            if conda_exe_in(root).is_some() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        return Err("Miniconda 安装超时（3 分钟内未出现 conda），请重试".to_string());
    }
    #[cfg(not(windows))]
    {
        run_capture(
            std::process::Command::new("sh")
                .arg(installer)
                .args(["-b", "-p"])
                .arg(root),
            "安装 Miniconda",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// conda 布局：根目录下按平台放 conda 可执行文件与 envs/<name>/python。
    fn fake_conda(root: &Path, with_env: bool) {
        let (exe, py): (&str, &str) = if cfg!(windows) {
            ("Scripts/conda.exe", "python.exe")
        } else {
            ("bin/conda", "bin/python")
        };
        let exe_path = root.join(exe);
        std::fs::create_dir_all(exe_path.parent().unwrap()).unwrap();
        std::fs::write(&exe_path, b"conda").unwrap();
        if with_env {
            let py_path = root.join("envs").join(ENV_NAME).join(py);
            std::fs::create_dir_all(py_path.parent().unwrap()).unwrap();
            std::fs::write(&py_path, b"python").unwrap();
        }
    }

    #[test]
    fn resolve_prefers_root_with_env() {
        let tmp = tempfile::tempdir().unwrap();
        let with_env = tmp.path().join("with-env");
        let without_env = tmp.path().join("without-env");
        fake_conda(&with_env, true);
        fake_conda(&without_env, false);
        // 无环境在前：有环境的 root 仍应胜出。
        let env = resolve_with_roots(tmp.path(), &[without_env, with_env.clone()]);
        assert!(env.ready());
        assert_eq!(env.python.unwrap(), env_python_in(&with_env).unwrap());
    }

    #[test]
    fn resolve_reports_conda_without_env() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("conda");
        fake_conda(&root, false);
        let env = resolve_with_roots(tmp.path(), &[root]);
        assert!(!env.ready());
        assert!(env.conda.is_some());
    }

    #[test]
    fn resolve_empty_without_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let env = resolve_with_roots(tmp.path(), &[]);
        assert!(!env.ready());
        assert!(env.conda.is_none());
    }

    /// envs-only 目录（~/.conda 布局）也能命中，conda 取自其它 root。
    #[test]
    fn resolve_pairs_envs_only_dir_with_foreign_conda() {
        let tmp = tempfile::tempdir().unwrap();
        let conda_root = tmp.path().join("conda");
        fake_conda(&conda_root, false);
        let envs_only = tmp.path().join("dotconda");
        // 只造 envs/<name>/python，不造 bin/conda。
        let py: &str = if cfg!(windows) { "python.exe" } else { "bin/python" };
        let py_path = envs_only.join("envs").join(ENV_NAME).join(py);
        std::fs::create_dir_all(py_path.parent().unwrap()).unwrap();
        std::fs::write(&py_path, b"python").unwrap();
        let env = resolve_with_roots(tmp.path(), &[envs_only, conda_root.clone()]);
        assert!(env.ready());
        assert_eq!(env.conda.unwrap(), conda_exe_in(&conda_root).unwrap());
    }

    #[test]
    fn prompt_block_states() {
        let ready = SkillEnv {
            conda: Some(PathBuf::from("/c/bin/conda")),
            python: Some(PathBuf::from("/c/envs/siku-office/bin/python")),
            app_data_dir: PathBuf::new(),
        };
        let block = prompt_block(&ready);
        assert!(block.contains("已就绪") && block.contains("/c/envs"));

        let conda_only = SkillEnv {
            conda: Some(PathBuf::from("/c/bin/conda")),
            python: None,
            app_data_dir: PathBuf::new(),
        };
        let block = prompt_block(&conda_only);
        assert!(block.contains("未就绪") && block.contains("skill_env_setup"));
        assert!(block.contains("已有 conda"));

        let none = SkillEnv::default();
        let block = prompt_block(&none);
        assert!(block.contains("未检测到 conda") && block.contains("skill_env_setup"));
    }
}
