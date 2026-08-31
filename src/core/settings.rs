// 配置数据模型与 TOML 持久化（暗色主题 / 设置页面功能的基础数据层）
//
// 依赖方向：ui → core → sys。本模块只依赖标准库 + serde/toml/anyhow，
// 不触碰任何 Win32 API，可独立单测。

use anyhow::Context;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// 主题模式（供设置页下拉框选择）
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThemeMode {
    /// 跟随系统主题（默认）
    System,
    /// 浅色主题
    Light,
    /// 深色主题
    Dark,
}

impl ThemeMode {
    /// 返回主题的中文标签（供设置页下拉框展示）
    pub const fn label_cn(&self) -> &'static str {
        match self {
            ThemeMode::System => "跟随系统",
            ThemeMode::Light => "浅色",
            ThemeMode::Dark => "深色",
        }
    }
}

/// 窗口圆角偏好（供设置页下拉框选择）
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CornerPreference {
    /// 默认圆角（跟随系统/默认值）
    Default,
    /// 大圆角
    Round,
    /// 小圆角
    SmallRound,
}

impl CornerPreference {
    /// 返回圆角偏好的中文标签（供设置页下拉框展示）
    pub const fn label_cn(&self) -> &'static str {
        match self {
            CornerPreference::Default => "默认",
            CornerPreference::Round => "圆角",
            CornerPreference::SmallRound => "小圆角",
        }
    }
}

/// 返回 `true`（供 `show_badge_title` 的 serde 缺省值使用）
///
/// 旧版 config.toml 缺少该字段时按"显示标题"回退（R6 默认行为）。
fn default_true() -> bool {
    true
}

/// 应用设置（内存中配置数据模型，可序列化为 TOML）
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// 主题模式
    pub theme: ThemeMode,
    /// 窗口圆角偏好
    pub corner: CornerPreference,
    /// 角标上是否显示标题文字（R6；缺省 true，旧配置文件缺字段时按显示回退）
    #[serde(default = "default_true")]
    pub show_badge_title: bool,
    /// 角标是否始终置顶（R19；缺省 true，旧配置文件缺字段时按置顶回退）。
    /// 关闭后角标跟随目标窗口 z 序：被其他窗口盖住时随之隐藏。
    #[serde(default = "default_true")]
    pub badge_always_top: bool,
}

impl Default for Settings {
    /// 默认配置：跟随系统主题 + 默认圆角 + 角标显示标题
    fn default() -> Self {
        Settings {
            theme: ThemeMode::System,
            corner: CornerPreference::Default,
            show_badge_title: true,
            badge_always_top: true,
        }
    }
}

/// 全局设置单例（镜像 GLOBAL_TAG_STORE 模式：主线程注入，WndProc/设置页共享）
static GLOBAL_SETTINGS: OnceLock<Arc<Mutex<Settings>>> = OnceLock::new();

/// 注入全局设置（重复调用仅首次生效，与 `GLOBAL_TAG_STORE` 注入惯例一致）
pub fn set_global_settings(s: Arc<Mutex<Settings>>) {
    let _ = GLOBAL_SETTINGS.set(s);
}

/// 获取全局设置（未注入时返回 None，调用方自行回退默认值）
pub fn global_settings() -> Option<Arc<Mutex<Settings>>> {
    GLOBAL_SETTINGS.get().cloned()
}

/// 按优先级选择配置根目录：CLI > 环境变量 > exe 探测 > APPDATA。
///
/// 纯函数：四个候选按优先级取第一个 `Some`，全部为 `None` 时返回 `None`
/// （由调用方决定兜底）。候选值本身不做存在性/可写性校验。
pub(crate) fn pick_config_root(
    cli: Option<&Path>,
    env: Option<&Path>,
    exe_probe: Option<&Path>,
    appdata: Option<&Path>,
) -> Option<PathBuf> {
    cli.or(env)
        .or(exe_probe)
        .or(appdata)
        .map(|p| p.to_path_buf())
}

/// 从命令行参数解析 `--config-dir <dir>` 或 `--config-dir=<dir>`。
///
/// 使用 `OsString` 遍历（非 UTF-8 argv 不 panic，仅跳过前缀匹配）；
/// 未提供或 `--config-dir` 位于末尾缺值时返回 `None`。
pub fn parse_cli_config_dir(args: &[std::ffi::OsString]) -> Option<PathBuf> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--config-dir" {
            return iter.next().map(PathBuf::from);
        }
        if let Some(rest) = arg.to_str().and_then(|s| s.strip_prefix("--config-dir=")) {
            return Some(PathBuf::from(rest));
        }
    }
    None
}

/// 探测 `<exe_dir>/config` 作为配置根目录（D22 便携配置）。
///
/// 目录已存在直接采用；否则尝试 `create_dir_all` 探测可写性，
/// 成功（目录已建好，后续 save 可写入）返回 `Some`，失败返回 `None`。
pub(crate) fn probe_exe_config(exe_dir: &Path) -> Option<PathBuf> {
    let candidate = exe_dir.join("config");
    if candidate.exists() {
        return Some(candidate);
    }
    std::fs::create_dir_all(&candidate).ok()?;
    Some(candidate)
}

/// 判定 load 是否需读穿透旧配置（D9：只读不复制）。
///
/// 仅当 resolved 文件不存在且与 legacy 不是同一路径时为 `true`；
/// resolved == legacy 时为 `false`，避免对同一路径做二次无意义读取。
pub(crate) fn should_read_through(resolved: &Path, legacy: &Path) -> bool {
    !resolved.exists() && resolved != legacy
}

/// 旧版配置路径（D9 迁移前）：`%APPDATA%\WinTag\config.toml`。
///
/// 独立于解析链、仅用于 `load` 读穿透，绝不参与写入；
/// `APPDATA` 缺失时返回 `None`。
fn legacy_appdata_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|v| PathBuf::from(v).join("WinTag").join("config.toml"))
}

/// 配置根目录解析链（D22/R1）memoize：读/写共用同一结果，保证一致。
///
/// 优先级：`--config-dir` CLI > `WINTAG_CONFIG_DIR` env > `<exe_dir>/config`
/// 可写探测 > `%APPDATA%\WinTag`；全部不可得时兜底当前目录 `.`
/// （配置文件落为 `./config.toml`）。
static CONFIG_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 返回 memoize 后的配置根目录（首次调用执行解析链，见 [`CONFIG_ROOT`]）
pub fn config_root() -> PathBuf {
    CONFIG_ROOT
        .get_or_init(|| {
            let cli = parse_cli_config_dir(&std::env::args_os().collect::<Vec<_>>());
            let env = std::env::var_os("WINTAG_CONFIG_DIR").map(PathBuf::from);
            let exe = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().and_then(probe_exe_config));
            let appdata = std::env::var_os("APPDATA").map(|v| PathBuf::from(v).join("WinTag"));
            pick_config_root(
                cli.as_deref(),
                env.as_deref(),
                exe.as_deref(),
                appdata.as_deref(),
            )
            .unwrap_or_else(|| PathBuf::from("."))
        })
        .clone()
}

/// 返回配置文件路径：`config_root()/config.toml`（解析链见 [`config_root`]）
pub fn config_path() -> PathBuf {
    config_root().join("config.toml")
}

/// 将 TOML 字符串解析为 [`Settings`]
///
/// 私有函数：拆出"路径→内容"的解析逻辑，便于单元测试直接构造
/// 损坏/缺字段内容验证回退行为，无需依赖固定配置文件路径。
fn parse_str(s: &str) -> Result<Settings, toml::de::Error> {
    toml::from_str(s)
}

/// 解析配置内容，失败回退默认并告警（`load` 的公共尾段，避免复制粘贴）
fn parse_or_default(content: &str) -> Settings {
    match parse_str(content) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[配置] 读取配置失败，使用默认配置: {e}");
            Settings::default()
        }
    }
}

/// 从配置文件加载设置
///
/// 任何错误（文件不存在 / IO 失败 / TOML 解析失败）都回退到默认配置，
/// 仅打印一条中文警告，绝不 panic、绝不中断程序。
///
/// D9 读穿透：`config_path()` 不存在时，best-effort 读旧
/// `%APPDATA%\WinTag\config.toml`（仅当二者不是同一路径，且不复制）。
pub fn load() -> Settings {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => parse_or_default(&content),
        Err(e) => {
            // 新位置无文件时读穿透旧 %APPDATA% 配置（D9：只读，不复制）
            if let Some(legacy) = legacy_appdata_path() {
                if should_read_through(&path, &legacy) {
                    if let Ok(content) = std::fs::read_to_string(&legacy) {
                        println!("[配置] 已从旧位置 %APPDATA% 读取（D9 不复制）");
                        return parse_or_default(&content);
                    }
                }
            }
            eprintln!("[配置] 读取配置失败，使用默认配置: {e}");
            Settings::default()
        }
    }
}

impl Settings {
    /// 保存设置到默认配置文件（`config_path()`），父目录不存在时自动创建
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        self.save_to(&path)
    }

    /// 保存设置到指定路径（供 `save` 与单元测试注入路径复用）
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        // 先创建父目录：%APPDATA%\WinTag 首次运行时可能不存在
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("序列化配置失败")?;
        std::fs::write(path, content)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 (a)：serde 往返 —— 每个枚举变体（内嵌于 Settings）+ 完整配置序列化后反序列化一致
    ///（TOML 文档顶层必须是表，因此枚举变体经 Settings 结构体做往返）
    #[test]
    fn test_serde_roundtrip() {
        for theme in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
            let cfg = Settings {
                theme,
                corner: CornerPreference::Default,
                show_badge_title: true,
                badge_always_top: true,
            };
            let s = toml::to_string(&cfg).unwrap();
            let back: Settings = toml::from_str(&s).unwrap();
            assert_eq!(back, cfg);
        }
        for corner in [
            CornerPreference::Default,
            CornerPreference::Round,
            CornerPreference::SmallRound,
        ] {
            let cfg = Settings {
                theme: ThemeMode::System,
                corner,
                show_badge_title: false,
                badge_always_top: false,
            };
            let s = toml::to_string(&cfg).unwrap();
            let back: Settings = toml::from_str(&s).unwrap();
            assert_eq!(back, cfg);
        }
        // 完整混合配置往返
        let cfg = Settings {
            theme: ThemeMode::Dark,
            corner: CornerPreference::Round,
            show_badge_title: false,
            badge_always_top: false,
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: Settings = toml::from_str(&s).unwrap();
        assert_eq!(back, cfg);
    }

    /// 测试 (b)：Default 值正确（theme=System, corner=Default, show_badge_title=true）
    #[test]
    fn test_default_values() {
        let d = Settings::default();
        assert_eq!(d.theme, ThemeMode::System);
        assert_eq!(d.corner, CornerPreference::Default);
        assert!(d.show_badge_title);
        assert!(d.badge_always_top);
    }

    /// 测试 (c)：损坏/缺必需字段/空内容均解析失败 → load 走默认回退路径
    #[test]
    fn test_parse_corrupt_fallback() {
        assert!(parse_str("this is not toml {{{").is_err());
        // 缺 corner 字段的合法 TOML 同样解析失败
        assert!(parse_str("theme = \"Dark\"").is_err());
        // 空内容也解析失败（结构体字段无缺省值）
        assert!(parse_str("").is_err());
    }

    /// 测试 (c2)：旧版配置缺 show_badge_title 字段可解析，回退 true（R6 兼容）
    #[test]
    fn test_parse_legacy_config_missing_show_badge_title() {
        let legacy = "theme = \"Dark\"\ncorner = \"Round\"\n";
        let parsed: Settings = parse_str(legacy).unwrap();
        assert_eq!(parsed.theme, ThemeMode::Dark);
        assert_eq!(parsed.corner, CornerPreference::Round);
        assert!(parsed.show_badge_title, "缺省字段应回退为显示标题");
    }

    /// 测试 (c3)：旧版配置缺 badge_always_top 字段可解析，回退 true（R19 兼容）
    #[test]
    fn test_parse_legacy_config_missing_badge_always_top() {
        let legacy = "theme = \"Dark\"\ncorner = \"Round\"\nshow_badge_title = false\n";
        let parsed: Settings = parse_str(legacy).unwrap();
        assert_eq!(parsed.theme, ThemeMode::Dark);
        assert!(!parsed.show_badge_title);
        assert!(parsed.badge_always_top, "缺省字段应回退为始终置顶");
    }

    /// 测试 (d)：save→load 往返 —— 写入临时路径，验证文件内容含枚举关键值并可读回
    #[test]
    fn test_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wintag_settings_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let cfg = Settings {
            theme: ThemeMode::Dark,
            corner: CornerPreference::SmallRound,
            show_badge_title: true,
            badge_always_top: false,
        };
        cfg.save_to(&path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("theme = \"Dark\""), "内容: {content}");
        assert!(
            content.contains("corner = \"SmallRound\""),
            "内容: {content}"
        );

        let back: Settings = parse_str(&content).unwrap();
        assert_eq!(back, cfg);

        let _ = std::fs::remove_file(&path);
    }

    /// 测试 (e)：中文标签（供设置页下拉框使用）
    #[test]
    fn test_label_cn() {
        assert_eq!(ThemeMode::System.label_cn(), "跟随系统");
        assert_eq!(ThemeMode::Light.label_cn(), "浅色");
        assert_eq!(ThemeMode::Dark.label_cn(), "深色");
        assert_eq!(CornerPreference::Default.label_cn(), "默认");
        assert_eq!(CornerPreference::Round.label_cn(), "圆角");
        assert_eq!(CornerPreference::SmallRound.label_cn(), "小圆角");
    }

    // ---------- D22/R1 配置路径解析链 ----------

    /// 测试 (f1)：pick_config_root 全 None → None
    #[test]
    fn test_pick_config_root_all_none() {
        assert_eq!(pick_config_root(None, None, None, None), None);
    }

    /// 测试 (f2)：pick_config_root 优先级 —— cli > env > exe_probe > appdata
    #[test]
    fn test_pick_config_root_priority() {
        let cli = Path::new("C:/cli");
        let env = Path::new("C:/env");
        let exe = Path::new("C:/exe");
        let app = Path::new("C:/appdata");
        // cli 优先于其余三者
        assert_eq!(
            pick_config_root(Some(cli), Some(env), Some(exe), Some(app)),
            Some(PathBuf::from("C:/cli"))
        );
        // 无 cli 时 env 优先于 exe_probe / appdata
        assert_eq!(
            pick_config_root(None, Some(env), Some(exe), Some(app)),
            Some(PathBuf::from("C:/env"))
        );
        // 无 cli/env 时 exe_probe 优先于 appdata
        assert_eq!(
            pick_config_root(None, None, Some(exe), Some(app)),
            Some(PathBuf::from("C:/exe"))
        );
        // 仅 appdata
        assert_eq!(
            pick_config_root(None, None, None, Some(app)),
            Some(PathBuf::from("C:/appdata"))
        );
    }

    /// 测试 (g1)：parse_cli_config_dir —— `--config-dir <dir>` / `--config-dir=<dir>` / 缺省 / 无值
    #[test]
    fn test_parse_cli_config_dir_forms() {
        use std::ffi::OsString;
        // 空格分隔形式
        let args = [
            OsString::from("prog"),
            OsString::from("--config-dir"),
            OsString::from("D:\\cfg"),
        ];
        assert_eq!(parse_cli_config_dir(&args), Some(PathBuf::from("D:\\cfg")));
        // 等号形式
        let args = [
            OsString::from("prog"),
            OsString::from("--config-dir=D:\\cfg2"),
        ];
        assert_eq!(parse_cli_config_dir(&args), Some(PathBuf::from("D:\\cfg2")));
        // 缺省 → None
        let args = [OsString::from("prog"), OsString::from("--help")];
        assert_eq!(parse_cli_config_dir(&args), None);
        // --config-dir 位于末尾无值 → None（不 panic）
        let args = [OsString::from("prog"), OsString::from("--config-dir")];
        assert_eq!(parse_cli_config_dir(&args), None);
    }

    /// 测试 (g2)：parse_cli_config_dir —— 非 UTF-8 OsString 不 panic
    #[test]
    fn test_parse_cli_config_dir_non_utf8_no_panic() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        // 未配对 UTF-16 代理项：合法 OsString 但非 UTF-8
        let weird = OsString::from_wide(&[0xD800]);
        let args = [
            OsString::from("prog"),
            weird,
            OsString::from("--config-dir"),
            OsString::from("E:\\cfg"),
        ];
        assert_eq!(parse_cli_config_dir(&args), Some(PathBuf::from("E:\\cfg")));
        // 非 UTF-8 值本身亦可作为路径值返回（不 panic、不参与 ASCII 前缀匹配）
        let weird2 = OsString::from_wide(&[0xD800, b'x' as u16]);
        let args2 = [OsString::from("--config-dir"), weird2];
        assert_eq!(parse_cli_config_dir(&args2), Some(PathBuf::from(&args2[1])));
    }

    /// 测试 (h)：should_read_through —— resolved 不存在且 ≠ legacy → true；相等或已存在 → false
    #[test]
    fn test_should_read_through() {
        let base = std::env::temp_dir().join(format!("wintag_readthrough_{}", std::process::id()));
        let resolved = base.join("resolved.toml");
        let legacy = base.join("legacy.toml");
        // resolved 不存在且不等于 legacy → true
        assert!(should_read_through(&resolved, &legacy));
        // resolved == legacy → false（避免重复读同一路径）
        assert!(!should_read_through(&resolved, &resolved));
        // resolved 已存在 → false
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&resolved, "x").unwrap();
        assert!(!should_read_through(&resolved, &legacy));
        let _ = std::fs::remove_dir_all(&base);
    }
}
