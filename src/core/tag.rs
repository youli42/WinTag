use std::collections::HashMap;

/// 标签颜色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TagColor {
    Orange,
    Blue,
    Green,
    Red,
    Purple,
}

impl TagColor {
    #[allow(dead_code)]
    pub fn as_rgba(&self) -> [u8; 4] {
        match self {
            TagColor::Orange => [255, 183, 77, 255],
            TagColor::Blue => [66, 165, 245, 255],
            TagColor::Green => [102, 187, 106, 255],
            TagColor::Red => [239, 83, 80, 255],
            TagColor::Purple => [171, 71, 188, 255],
        }
    }
}

/// 窗口标签数据
#[derive(Debug, Clone)]
pub struct Tag {
    pub title: String,
    pub note: String,
    #[allow(dead_code)]
    pub color: TagColor,
    pub window_title: String,
    pub process_name: String,
}

/// 内存标签存储，以 HWND 为键
pub type TagStore = HashMap<isize, Tag>;
