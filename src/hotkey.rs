use anyhow::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, MOD_CONTROL, MOD_SHIFT, VK_M, VK_N, VK_S,
};
use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;

/// 热键定义
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// Ctrl+Shift+N — 快速标记当前窗口
    QuickTag,
    /// Ctrl+Shift+M — 打开概览面板
    TogglePanel,
    /// Ctrl+Shift+S — 打开设置页面
    OpenSettings,
}

impl Hotkey {
    pub const fn id(&self) -> i32 {
        match self {
            Hotkey::QuickTag => 1,
            Hotkey::TogglePanel => 2,
            Hotkey::OpenSettings => 3,
        }
    }

    pub const fn modifiers(&self) -> u32 {
        // MOD_* 常量本身已是 u32，无需再 cast
        MOD_CONTROL.0 | MOD_SHIFT.0
    }

    pub const fn vk(&self) -> u32 {
        // windows 0.58 中 VK_* 常量为 u16，需转 u32 匹配 RegisterHotKey 签名
        match self {
            Hotkey::QuickTag => VK_N.0 as u32,
            Hotkey::TogglePanel => VK_M.0 as u32,
            Hotkey::OpenSettings => VK_S.0 as u32,
        }
    }

    /// 根据热键 ID 解析对应的热键变体
    ///
    /// 遍历所有热键变体，找到 `id()` 与传入值相等的变体并返回；
    /// 若没有匹配的变体则返回 `None`。
    pub const fn from_id(id: i32) -> Option<Hotkey> {
        if id == Hotkey::QuickTag.id() {
            return Some(Hotkey::QuickTag);
        }
        if id == Hotkey::TogglePanel.id() {
            return Some(Hotkey::TogglePanel);
        }
        if id == Hotkey::OpenSettings.id() {
            return Some(Hotkey::OpenSettings);
        }
        None
    }
}

/// 注册所有全局热键，挂载到指定窗口
///
/// 采用尽力而为策略：逐个注册热键，单个注册失败（如与其他程序冲突）
/// 时打印警告并继续注册剩余热键，不中断程序运行。
pub fn register_all(hwnd: HWND) -> Result<()> {
    for hotkey in &[Hotkey::QuickTag, Hotkey::TogglePanel, Hotkey::OpenSettings] {
        // SAFETY: RegisterHotKey 注册全局热键，参数由常量定义
        let result = unsafe {
            RegisterHotKey(
                hwnd,
                hotkey.id(),
                windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(hotkey.modifiers()),
                hotkey.vk(),
            )
        };
        if let Err(e) = result {
            eprintln!("[热键] 注册失败 (id={}): {}", hotkey.id(), e);
        }
    }
    Ok(())
}

/// 从 WM_HOTKEY 消息参数解析热键类型
pub fn from_message(msg: u32, wparam: usize, _lparam: isize) -> Option<Hotkey> {
    if msg != WM_HOTKEY {
        return None;
    }
    Hotkey::from_id(wparam as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 遍历所有热键变体，验证 from_id 与 id 互为往返映射
    #[test]
    fn test_hotkey_from_id_roundtrip() {
        for hk in [Hotkey::QuickTag, Hotkey::TogglePanel, Hotkey::OpenSettings] {
            assert_eq!(Hotkey::from_id(hk.id()), Some(hk));
        }
        assert_eq!(Hotkey::from_id(999), None);
    }

    /// 验证 OpenSettings 变体的常量值（id=3, Ctrl+Shift+S）
    #[test]
    fn test_hotkey_open_settings_constants() {
        assert_eq!(Hotkey::OpenSettings.id(), 3);
        assert_eq!(Hotkey::OpenSettings.vk(), VK_S.0 as u32);
        assert_eq!(Hotkey::OpenSettings.modifiers(), 0x0006);
        assert_eq!(Hotkey::from_id(3), Some(Hotkey::OpenSettings));
    }
}
