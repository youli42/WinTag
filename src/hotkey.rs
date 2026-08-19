use anyhow::Result;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, MOD_CONTROL, MOD_SHIFT, VK_N, VK_M,
};
use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;

/// 热键定义
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hotkey {
    /// Ctrl+Shift+N — 快速标记当前窗口
    QuickTag,
    /// Ctrl+Shift+M — 打开概览面板
    TogglePanel,
}

impl Hotkey {
    pub const fn id(&self) -> i32 {
        match self {
            Hotkey::QuickTag => 1,
            Hotkey::TogglePanel => 2,
        }
    }

    pub const fn modifiers(&self) -> u32 {
        (MOD_CONTROL.0 | MOD_SHIFT.0) as u32
    }

    pub const fn vk(&self) -> u32 {
        match self {
            Hotkey::QuickTag => VK_N.0 as u32,
            Hotkey::TogglePanel => VK_M.0 as u32,
        }
    }
}

/// 注册所有全局热键
pub fn register_all() -> Result<()> {
    for hotkey in &[Hotkey::QuickTag, Hotkey::TogglePanel] {
        // SAFETY: RegisterHotKey 注册全局热键，参数由常量定义
        unsafe {
            RegisterHotKey(
                None,
                hotkey.id(),
                windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS(
                    hotkey.modifiers(),
                ),
                hotkey.vk(),
            )
        }
        .map_err(|e| anyhow::anyhow!("注册热键失败 (id={}): {}", hotkey.id(), e))?;
    }
    Ok(())
}

/// 从 WM_HOTKEY 消息参数解析热键类型
pub fn from_message(msg: u32, wparam: usize, _lparam: isize) -> Option<Hotkey> {
    if msg != WM_HOTKEY {
        return None;
    }
    match wparam {
        1 => Some(Hotkey::QuickTag),
        2 => Some(Hotkey::TogglePanel),
        _ => None,
    }
}