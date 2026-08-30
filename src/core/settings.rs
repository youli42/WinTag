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

/// 返回配置文件路径：`%APPDATA%\WinTag\config.toml`
///
/// 若 `APPDATA` 环境变量缺失（如精简版 Windows 或测试环境），
/// 回退到当前工作目录下的 `./wintag.toml`。
pub fn config_path() -> PathBuf {
    match std::env::var("APPDATA") {
        Ok(dir) => PathBuf::from(dir).join("WinTag").join("config.toml"),
        Err(_) => PathBuf::from("./wintag.toml"),
    }
}

/// 将 TOML 字符串解析为 [`Settings`]
///
/// 私有函数：拆出"路径→内容"的解析逻辑，便于单元测试直接构造
/// 损坏/缺字段内容验证回退行为，无需依赖固定配置文件路径。
fn parse_str(s: &str) -> Result<Settings, toml::de::Error> {
    toml::from_str(s)
}

/// 从配置文件加载设置
///
/// 任何错误（文件不存在 / IO 失败 / TOML 解析失败）都回退到默认配置，
/// 仅打印一条中文警告，绝不 panic、绝不中断程序。
pub fn load() -> Settings {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => match parse_str(&content) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[配置] 读取配置失败，使用默认配置: {e}");
                Settings::default()
            }
        },
        Err(e) => {
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
}
