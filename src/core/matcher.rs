use crate::core::tag::{Tag, TagStore};

/// 根据窗口句柄查找标签
#[allow(dead_code)]
pub fn find_tag(store: &TagStore, hwnd: isize) -> Option<&Tag> {
    store.get(&hwnd)
}

/// 根据窗口句柄查找标签（可变引用）
#[allow(dead_code)]
pub fn find_tag_mut(store: &mut TagStore, hwnd: isize) -> Option<&mut Tag> {
    store.get_mut(&hwnd)
}

/// 添加或更新标签
pub fn upsert_tag(store: &mut TagStore, hwnd: isize, tag: Tag) {
    store.insert(hwnd, tag);
}

/// 移除标签
#[allow(dead_code)]
pub fn remove_tag(store: &mut TagStore, hwnd: isize) -> Option<Tag> {
    store.remove(&hwnd)
}

/// 清理无效句柄的标签
#[allow(dead_code)]
pub fn cleanup_invalid(store: &mut TagStore, valid_hwnds: &[isize]) {
    store.retain(|hwnd, _| valid_hwnds.contains(hwnd));
}
