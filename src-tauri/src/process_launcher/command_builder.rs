//! ComfyUI 启动命令构造器
//!
//! 详见 `PR/03-模块设计/06-ProcessLauncher.md §4.2 启动参数构造`
//!
//! 设计要点：
//! - 不走 shell（spawn 直接执行），避免 shell 注入
//! - `sanitize_custom_args` 过滤危险字符（防 ComfyUI 误解析）
//! - 复用 `LaunchMode::to_args()` 和 `PreviewMethod::to_arg()` 减少冗余

use crate::process_launcher::models::LaunchArgs;

/// 构造 ComfyUI 启动命令参数向量
///
/// 返回的 Vec 不包含 python 二进制路径本身（由调用方 prepend），
/// 第一个元素固定为 "main.py"。
///
/// # 参数顺序
/// 1. `main.py`
/// 2. 显存策略（mode → `--cpu --lowvram` / `--highvram` / `--lowvram` / `--novram`）
/// 3. Custom 模式的用户参数（经 `sanitize_custom_args` 过滤）
/// 4. 公共参数：`--listen <host>` / `--port <port>` / `--preview-method <method>`
/// 5. `--auto-launch`（可选）
/// 6. 高级参数：`--use-split-cross-attention` / `--force-fp32` 等
pub fn build_command(args: &LaunchArgs) -> Vec<String> {
    let mut cmd: Vec<String> = vec!["main.py".into()];

    // v3.x：--base-directory（custom_nodes 在 ComfyUI 外时使用）
    // 见 LaunchArgs::base_directory 字段说明
    if let Some(base_dir) = &args.base_directory {
        cmd.push("--base-directory".into());
        cmd.push(base_dir.to_string_lossy().to_string());
    }

    // 显存策略（复用 LaunchMode::to_args）
    cmd.extend(args.mode.to_args().iter().map(|s| s.to_string()));

    // Custom 模式追加用户参数
    if matches!(args.mode, crate::config::LaunchMode::Custom) {
        if let Some(custom) = &args.custom_args {
            let filtered = sanitize_custom_args(custom);
            cmd.extend(filtered);
        }
    }

    // 公共参数
    cmd.push("--listen".into());
    cmd.push(args.listen_host.clone());
    cmd.push("--port".into());
    cmd.push(args.listen_port.to_string());
    cmd.push("--preview-method".into());
    cmd.push(args.preview_method.to_arg().into());

    // 自动打开浏览器
    if args.auto_launch {
        cmd.push("--auto-launch".into());
    }

    // 高级参数
    let adv = &args.advanced;
    if adv.use_split_cross_attention {
        cmd.push("--use-split-cross-attention".into());
    }
    if adv.use_pytorch_cross_attention {
        cmd.push("--use-pytorch-cross-attention".into());
    }
    if adv.force_fp32 {
        cmd.push("--force-fp32".into());
    }
    if adv.fp16_vae {
        cmd.push("--fp16-vae".into());
    }
    if adv.bf16_vae {
        cmd.push("--bf16-vae".into());
    }
    if adv.no_half {
        cmd.push("--no-half".into());
    }
    if adv.no_half_vae {
        cmd.push("--no-half-vae".into());
    }
    if adv.directml {
        cmd.push("--directml".into());
    }

    cmd
}

/// 过滤 custom_args 中的危险字符
///
/// ComfyUI 通过 `sys.argv` 接收参数，spawn 不走 shell，但仍需防
/// 用户误填 `; rm -rf /` 之类内容（虽然不会执行，但会污染参数解析）。
///
/// # 规则
/// - 按空白切分
/// - 拒绝包含 shell 元字符（`;` `|` `&` 反引号 `$` `>` `<`）的 token
/// - 拒绝 `-` 开头但非 `--` 开头的 token（防空参数名）
/// - 允许 `--` 开头的长参数名（如 `--disable-server-load`）
/// - 允许普通值（如 `autoencoder` / 数字 / 路径）
pub fn sanitize_custom_args(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .filter(|s| is_safe_token(s))
        .map(String::from)
        .collect()
}

/// 判断单个 token 是否安全
fn is_safe_token(s: &str) -> bool {
    // 拒绝 shell 元字符
    let has_meta = s.contains(';')
        || s.contains('|')
        || s.contains('&')
        || s.contains('`')
        || s.contains('$')
        || s.contains('>')
        || s.contains('<');

    if has_meta {
        return false;
    }

    // 允许 -- 开头的长参数
    if s.starts_with("--") {
        return true;
    }

    // 拒绝 - 开头的单短参数（如 -c）— ComfyUI 不支持单短参数
    if s.starts_with('-') {
        return false;
    }

    // 普通值
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AdvancedArgs, LaunchMode, PreviewMethod};

    fn make_args(mode: LaunchMode) -> LaunchArgs {
        LaunchArgs {
            mode,
            listen_host: "127.0.0.1".into(),
            listen_port: 8188,
            // v3.4.1 修复：旧版 Latent 已被移除，改用 Latent2Rgb
            preview_method: PreviewMethod::Latent2Rgb,
            auto_launch: false,
            advanced: AdvancedArgs::default(),
            custom_args: None,
            // v3.x：默认不传 --base-directory
            base_directory: None,
        }
    }

    #[test]
    fn test_build_command_cpu_mode() {
        let args = make_args(LaunchMode::Cpu);
        let cmd = build_command(&args);
        assert_eq!(cmd[0], "main.py");
        assert!(cmd.contains(&"--cpu".to_string()));
        assert!(cmd.contains(&"--lowvram".to_string()));
        assert!(cmd.contains(&"--listen".to_string()));
        assert!(cmd.contains(&"127.0.0.1".to_string()));
        assert!(cmd.contains(&"--port".to_string()));
        assert!(cmd.contains(&"8188".to_string()));
        assert!(!cmd.contains(&"--auto-launch".to_string()));
    }

    #[test]
    fn test_build_command_gpu_high() {
        let args = make_args(LaunchMode::GpuHigh);
        let cmd = build_command(&args);
        assert!(cmd.contains(&"--highvram".to_string()));
        assert!(!cmd.contains(&"--lowvram".to_string()));
    }

    #[test]
    fn test_build_command_gpu_low() {
        let args = make_args(LaunchMode::GpuLow);
        let cmd = build_command(&args);
        assert!(cmd.contains(&"--lowvram".to_string()));
    }

    #[test]
    fn test_build_command_gpu_no_vram() {
        let args = make_args(LaunchMode::GpuNoVram);
        let cmd = build_command(&args);
        assert!(cmd.contains(&"--novram".to_string()));
    }

    #[test]
    fn test_build_command_auto_launch() {
        let mut args = make_args(LaunchMode::GpuHigh);
        args.auto_launch = true;
        let cmd = build_command(&args);
        assert!(cmd.contains(&"--auto-launch".to_string()));
    }

    #[test]
    fn test_build_command_advanced_flags() {
        let mut args = make_args(LaunchMode::GpuHigh);
        args.advanced.force_fp32 = true;
        args.advanced.use_split_cross_attention = true;
        let cmd = build_command(&args);
        assert!(cmd.contains(&"--force-fp32".to_string()));
        assert!(cmd.contains(&"--use-split-cross-attention".to_string()));
    }

    #[test]
    fn test_build_command_custom_mode_with_safe_args() {
        let mut args = make_args(LaunchMode::Custom);
        args.custom_args = Some("--disable-server-load --quick-test-for-listen".into());
        let cmd = build_command(&args);
        // 验证 safe -- 参数被加入
        assert!(cmd.contains(&"--disable-server-load".to_string()));
        assert!(cmd.contains(&"--quick-test-for-listen".to_string()));
    }

    #[test]
    fn test_sanitize_filters_shell_metacharacters() {
        // 输入切分后：["--foo;rm", "-rf", "/", "--bar|cat"]
        // - "--foo;rm" 含 ; → 拒绝
        // - "-rf" 单短参数 → 拒绝
        // - "/" 普通值 → 通过
        // - "--bar|cat" 含 | → 拒绝
        let filtered = sanitize_custom_args("--foo;rm -rf / --bar|cat");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], "/");
        // 验证：含元字符的 token 均未通过
        assert!(filtered.iter().all(|s| !s.contains(';')));
        assert!(filtered.iter().all(|s| !s.contains('|')));
        assert!(filtered.iter().all(|s| !s.contains('&')));
    }

    #[test]
    fn test_sanitize_rejects_short_options() {
        let filtered = sanitize_custom_args("-c --valid");
        assert!(!filtered.contains(&"-c".to_string()));
        assert!(filtered.contains(&"--valid".to_string()));
    }

    #[test]
    fn test_sanitize_empty_input() {
        assert!(sanitize_custom_args("").is_empty());
        assert!(sanitize_custom_args("   \t\n  ").is_empty());
    }

    #[test]
    fn test_sanitize_allows_normal_values() {
        let filtered = sanitize_custom_args("autoencoder 1234 /path/to/something");
        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"autoencoder".to_string()));
        assert!(filtered.contains(&"1234".to_string()));
        assert!(filtered.contains(&"/path/to/something".to_string()));
    }

    // v3.x：--base-directory 行为测试
    #[test]
    fn test_build_command_with_base_directory() {
        let mut args = make_args(LaunchMode::GpuHigh);
        args.base_directory = Some(std::path::PathBuf::from("/custom/env"));
        let cmd = build_command(&args);
        // 验证 --base-directory 出现在 cmd 中
        let pos = cmd.iter().position(|s| s == "--base-directory");
        assert!(pos.is_some(), "--base-directory 应该被加入");
        assert_eq!(cmd[pos.unwrap() + 1], "/custom/env");
    }

    #[test]
    fn test_build_command_without_base_directory() {
        let args = make_args(LaunchMode::GpuHigh);
        let cmd = build_command(&args);
        // 默认不传 --base-directory
        assert!(!cmd.contains(&"--base-directory".to_string()));
    }
}
