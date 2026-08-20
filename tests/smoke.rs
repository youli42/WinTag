use std::collections::HashMap;
use wintag::core::matcher;
use wintag::core::tag::{Tag, TagColor, TagStore};
use wintag::hotkey::{self, Hotkey};
use wintag::sys::window;

// ============================================================
// TagColor 测试
// ============================================================

#[test]
fn test_tag_color_values() {
    assert_eq!(TagColor::Orange.as_rgba(), [255, 183, 77, 255]);
    assert_eq!(TagColor::Blue.as_rgba(), [66, 165, 245, 255]);
    assert_eq!(TagColor::Green.as_rgba(), [102, 187, 106, 255]);
    assert_eq!(TagColor::Red.as_rgba(), [239, 83, 80, 255]);
    assert_eq!(TagColor::Purple.as_rgba(), [171, 71, 188, 255]);
}

#[test]
fn test_tag_color_debug_clone() {
    let c = TagColor::Orange;
    let c2 = c;
    assert_eq!(c, c2);
    assert_eq!(format!("{:?}", c), "Orange");
}

// ============================================================
// Tag 结构体测试
// ============================================================

#[test]
fn test_tag_create() {
    let tag = Tag {
        title: "修复 Bug".to_string(),
        note: "修复窗口关闭时的内存泄漏".to_string(),
        color: TagColor::Blue,
        window_title: "main.rs - VS Code".to_string(),
        process_name: "Code.exe".to_string(),
    };

    assert_eq!(tag.title, "修复 Bug");
    assert_eq!(tag.note, "修复窗口关闭时的内存泄漏");
    assert_eq!(tag.color, TagColor::Blue);
    assert_eq!(tag.window_title, "main.rs - VS Code");
    assert_eq!(tag.process_name, "Code.exe");
}

#[test]
fn test_tag_clone() {
    let tag = Tag {
        title: "测试".to_string(),
        note: String::new(),
        color: TagColor::Orange,
        window_title: "cmd".to_string(),
        process_name: "cmd.exe".to_string(),
    };
    let cloned = tag.clone();
    assert_eq!(tag.title, cloned.title);
    assert_eq!(tag.process_name, cloned.process_name);
}

#[test]
fn test_tag_empty_note() {
    let tag = Tag {
        title: "仅标题".to_string(),
        note: String::new(),
        color: TagColor::Green,
        window_title: "notepad".to_string(),
        process_name: "notepad.exe".to_string(),
    };
    assert!(tag.note.is_empty());
}

// ============================================================
// TagStore 测试
// ============================================================

#[test]
fn test_tag_store_empty() {
    let store: TagStore = HashMap::new();
    assert!(store.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn test_tag_store_insert_and_get() {
    let mut store: TagStore = HashMap::new();
    let tag = Tag {
        title: "任务 A".to_string(),
        note: String::new(),
        color: TagColor::Orange,
        window_title: "chrome".to_string(),
        process_name: "chrome.exe".to_string(),
    };

    store.insert(12345, tag.clone());
    assert_eq!(store.len(), 1);
    assert!(store.contains_key(&12345));

    let retrieved = store.get(&12345).unwrap();
    assert_eq!(retrieved.title, "任务 A");
    assert_eq!(retrieved.process_name, "chrome.exe");
}

#[test]
fn test_tag_store_multiple_windows() {
    let mut store: TagStore = HashMap::new();

    for i in 0..100 {
        store.insert(
            i as isize,
            Tag {
                title: format!("窗口 {}", i),
                note: String::new(),
                color: TagColor::Orange,
                window_title: format!("Window {}", i),
                process_name: "test.exe".to_string(),
            },
        );
    }

    assert_eq!(store.len(), 100);
    assert!(store.contains_key(&0));
    assert!(store.contains_key(&99));
}

#[test]
fn test_tag_store_overwrite() {
    let mut store: TagStore = HashMap::new();

    let tag1 = Tag {
        title: "旧标签".to_string(),
        note: String::new(),
        color: TagColor::Orange,
        window_title: "test".to_string(),
        process_name: "test.exe".to_string(),
    };

    let tag2 = Tag {
        title: "新标签".to_string(),
        note: "已更新".to_string(),
        color: TagColor::Blue,
        window_title: "test".to_string(),
        process_name: "test.exe".to_string(),
    };

    store.insert(12345, tag1);
    store.insert(12345, tag2);

    assert_eq!(store.len(), 1);
    let retrieved = store.get(&12345).unwrap();
    assert_eq!(retrieved.title, "新标签");
    assert_eq!(retrieved.note, "已更新");
}

#[test]
fn test_tag_store_remove() {
    let mut store: TagStore = HashMap::new();
    store.insert(
        12345,
        Tag {
            title: "删除我".to_string(),
            note: String::new(),
            color: TagColor::Orange,
            window_title: "test".to_string(),
            process_name: "test.exe".to_string(),
        },
    );

    let removed = store.remove(&12345);
    assert!(removed.is_some());
    assert!(store.is_empty());
    assert!(!store.contains_key(&12345));
}

// ============================================================
// Matcher 测试
// ============================================================

#[test]
fn test_matcher_upsert_and_find() {
    let mut store: TagStore = HashMap::new();
    let tag = Tag {
        title: "查找测试".to_string(),
        note: String::new(),
        color: TagColor::Red,
        window_title: "notepad".to_string(),
        process_name: "notepad.exe".to_string(),
    };

    matcher::upsert_tag(&mut store, 0x1000, tag);

    let found = matcher::find_tag(&store, 0x1000);
    assert!(found.is_some());
    assert_eq!(found.unwrap().title, "查找测试");

    let not_found = matcher::find_tag(&store, 0x9999);
    assert!(not_found.is_none());
}

#[test]
fn test_matcher_find_mut_and_modify() {
    let mut store: TagStore = HashMap::new();
    let tag = Tag {
        title: "原始标题".to_string(),
        note: String::new(),
        color: TagColor::Orange,
        window_title: "test".to_string(),
        process_name: "test.exe".to_string(),
    };

    matcher::upsert_tag(&mut store, 0x1000, tag);

    if let Some(t) = matcher::find_tag_mut(&mut store, 0x1000) {
        t.title = "修改后标题".to_string();
        t.note = "新增备注".to_string();
    }

    let found = matcher::find_tag(&store, 0x1000).unwrap();
    assert_eq!(found.title, "修改后标题");
    assert_eq!(found.note, "新增备注");
}

#[test]
fn test_matcher_remove_tag() {
    let mut store: TagStore = HashMap::new();
    let tag = Tag {
        title: "移除测试".to_string(),
        note: String::new(),
        color: TagColor::Orange,
        window_title: "test".to_string(),
        process_name: "test.exe".to_string(),
    };

    matcher::upsert_tag(&mut store, 0x1000, tag);
    assert_eq!(store.len(), 1);

    let removed = matcher::remove_tag(&mut store, 0x1000);
    assert!(removed.is_some());
    assert_eq!(store.len(), 0);

    let removed_again = matcher::remove_tag(&mut store, 0x1000);
    assert!(removed_again.is_none());
}

#[test]
fn test_matcher_cleanup_invalid() {
    let mut store: TagStore = HashMap::new();
    for i in 0..10 {
        let tag = Tag {
            title: format!("窗口 {}", i),
            note: String::new(),
            color: TagColor::Orange,
            window_title: format!("Window {}", i),
            process_name: "test.exe".to_string(),
        };
        matcher::upsert_tag(&mut store, i, tag);
    }
    assert_eq!(store.len(), 10);

    let valid: Vec<isize> = vec![0, 2, 4, 6, 8];
    matcher::cleanup_invalid(&mut store, &valid);
    assert_eq!(store.len(), 5);

    for hwnd in &valid {
        assert!(matcher::find_tag(&store, *hwnd).is_some());
    }
}

#[test]
fn test_matcher_upsert_updates_existing() {
    let mut store: TagStore = HashMap::new();
    let tag1 = Tag {
        title: "版本 1".to_string(),
        note: String::new(),
        color: TagColor::Orange,
        window_title: "test".to_string(),
        process_name: "test.exe".to_string(),
    };
    let tag2 = Tag {
        title: "版本 2".to_string(),
        note: "更新后的备注".to_string(),
        color: TagColor::Blue,
        window_title: "test".to_string(),
        process_name: "test.exe".to_string(),
    };

    matcher::upsert_tag(&mut store, 0x1000, tag1);
    matcher::upsert_tag(&mut store, 0x1000, tag2);

    assert_eq!(store.len(), 1);
    let found = matcher::find_tag(&store, 0x1000).unwrap();
    assert_eq!(found.title, "版本 2");
    assert_eq!(found.note, "更新后的备注");
}

// ============================================================
// Hotkey 测试
// ============================================================

#[test]
fn test_hotkey_ids() {
    assert_eq!(Hotkey::QuickTag.id(), 1);
    assert_eq!(Hotkey::TogglePanel.id(), 2);
}

#[test]
fn test_hotkey_vk() {
    // VK_N = 0x4E = 78, VK_M = 0x4D = 77
    assert_eq!(Hotkey::QuickTag.vk(), 78);
    assert_eq!(Hotkey::TogglePanel.vk(), 77);
}

#[test]
fn test_hotkey_modifiers() {
    // MOD_CONTROL = 0x0002, MOD_SHIFT = 0x0004
    assert_eq!(Hotkey::QuickTag.modifiers(), 0x0006);
    assert_eq!(Hotkey::TogglePanel.modifiers(), 0x0006);
}

#[test]
fn test_hotkey_from_message_quick_tag() {
    use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;
    let result = hotkey::from_message(WM_HOTKEY, 1, 0);
    assert_eq!(result, Some(Hotkey::QuickTag));
}

#[test]
fn test_hotkey_from_message_toggle_panel() {
    use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;
    let result = hotkey::from_message(WM_HOTKEY, 2, 0);
    assert_eq!(result, Some(Hotkey::TogglePanel));
}

#[test]
fn test_hotkey_from_message_wrong_msg() {
    let result = hotkey::from_message(0x0000, 1, 0); // 不是 WM_HOTKEY
    assert_eq!(result, None);
}

#[test]
fn test_hotkey_from_message_unknown_id() {
    use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;
    let result = hotkey::from_message(WM_HOTKEY, 999, 0);
    assert_eq!(result, None);
}

// ============================================================
// 窗口信息获取测试（在当前进程中可验证）
// ============================================================

#[test]
fn test_get_foreground_window_info_smoke() {
    let result = window::get_foreground_window_info();
    // 测试运行时，测试进程窗口是前台窗口
    assert!(result.is_ok(), "获取前台窗口信息应成功");
    let info = result.unwrap();
    // 至少应该有一个非零的 HWND
    assert_ne!(info.hwnd, 0, "HWND 不应为 0");
    // 进程名应该包含当前测试进程
    assert!(!info.process_name.is_empty(), "进程名不应为空");
}

#[test]
fn test_get_current_pid() {
    let pid = window::get_current_pid();
    assert!(pid > 0, "进程 PID 应大于 0");

    // 两次调用应返回相同 PID
    let pid2 = window::get_current_pid();
    assert_eq!(pid, pid2);
}

// ============================================================
// 集成测试：模拟完整标记流程
// ============================================================

#[test]
fn test_full_tag_flow() {
    let mut store: TagStore = HashMap::new();

    // 1. 模拟标记窗口
    let hwnd: isize = 0x1234;
    let window_title = "API 文档 - Chrome".to_string();
    let process_name = "chrome.exe".to_string();

    let tag = Tag {
        title: "阅读 API 文档".to_string(),
        note: "重点看认证部分".to_string(),
        color: TagColor::Green,
        window_title: window_title.clone(),
        process_name: process_name.clone(),
    };

    matcher::upsert_tag(&mut store, hwnd, tag);

    // 2. 验证已存储
    assert_eq!(store.len(), 1);
    let found = matcher::find_tag(&store, hwnd).unwrap();
    assert_eq!(found.title, "阅读 API 文档");
    assert_eq!(found.note, "重点看认证部分");
    assert_eq!(found.window_title, "API 文档 - Chrome");
    assert_eq!(found.process_name, "chrome.exe");

    // 3. 模拟修改标签
    let updated = Tag {
        title: "阅读 API 文档（已完成）".to_string(),
        note: "认证部分 OK，可以开始实现了".to_string(),
        color: TagColor::Blue,
        window_title: window_title.clone(),
        process_name: process_name.clone(),
    };
    matcher::upsert_tag(&mut store, hwnd, updated);

    // 4. 验证更新
    let found = matcher::find_tag(&store, hwnd).unwrap();
    assert_eq!(found.title, "阅读 API 文档（已完成）");
    assert_eq!(found.note, "认证部分 OK，可以开始实现了");
    assert_eq!(found.color, TagColor::Blue);

    // 5. 模拟窗口关闭 → 移除标签
    let removed = matcher::remove_tag(&mut store, hwnd);
    assert!(removed.is_some());
    assert!(store.is_empty());
    assert!(matcher::find_tag(&store, hwnd).is_none());
}

#[test]
fn test_multiple_windows_flow() {
    let mut store: TagStore = HashMap::new();

    let windows = vec![
        (
            0x1000,
            "main.rs - VS Code",
            "Code.exe",
            "修复登录 Bug",
            "处理空指针",
        ),
        (
            0x2000,
            "API 文档 - Chrome",
            "chrome.exe",
            "查 API 参数",
            "需要 Bearer token",
        ),
        (
            0x3000,
            "PowerShell",
            "WindowsTerminal.exe",
            "跑测试",
            "cargo test --all",
        ),
        (0x4000, "Notion", "Notion.exe", "整理需求", ""),
        (0x5000, "Slack", "slack.exe", "回复消息", "确认发布时间"),
    ];

    for (hwnd, wtitle, pname, title, note) in &windows {
        matcher::upsert_tag(
            &mut store,
            *hwnd,
            Tag {
                title: title.to_string(),
                note: note.to_string(),
                color: TagColor::Orange,
                window_title: wtitle.to_string(),
                process_name: pname.to_string(),
            },
        );
    }

    assert_eq!(store.len(), 5);

    // 搜索含 "API" 的标签
    let api_tags: Vec<_> = store.values().filter(|t| t.title.contains("API")).collect();
    assert_eq!(api_tags.len(), 1);
    assert_eq!(api_tags[0].window_title, "API 文档 - Chrome");

    // 搜索含 "测试" 的标签
    let test_tags: Vec<_> = store
        .values()
        .filter(|t| t.note.contains("cargo test"))
        .collect();
    assert_eq!(test_tags.len(), 1);
    assert_eq!(test_tags[0].process_name, "WindowsTerminal.exe");

    // 模拟关闭 Slack
    matcher::remove_tag(&mut store, 0x5000);
    assert_eq!(store.len(), 4);
    assert!(matcher::find_tag(&store, 0x5000).is_none());

    // 其余窗口仍然存在
    assert!(matcher::find_tag(&store, 0x1000).is_some());
    assert!(matcher::find_tag(&store, 0x2000).is_some());
    assert!(matcher::find_tag(&store, 0x3000).is_some());
    assert!(matcher::find_tag(&store, 0x4000).is_some());
}

// ============================================================
// 边界条件测试
// ============================================================

#[test]
fn test_empty_store_operations() {
    let mut store: TagStore = HashMap::new();

    assert!(matcher::find_tag(&store, 0x1000).is_none());
    assert!(matcher::find_tag_mut(&mut store, 0x1000).is_none());
    assert!(matcher::remove_tag(&mut store, 0x1000).is_none());

    // cleanup 空 store 不应 panic
    matcher::cleanup_invalid(&mut store, &[]);
    matcher::cleanup_invalid(&mut store, &[1, 2, 3]);
}

#[test]
fn test_cleanup_all_invalid() {
    let mut store: TagStore = HashMap::new();
    for i in 0..5 {
        matcher::upsert_tag(
            &mut store,
            i,
            Tag {
                title: format!("W {}", i),
                note: String::new(),
                color: TagColor::Orange,
                window_title: format!("W {}", i),
                process_name: "test.exe".to_string(),
            },
        );
    }
    assert_eq!(store.len(), 5);

    // 清理所有，只保留不存在的 HWND
    matcher::cleanup_invalid(&mut store, &[999, 1000]);
    assert_eq!(store.len(), 0);
}

#[test]
fn test_tag_with_unicode() {
    let tag = Tag {
        title: "日本語テスト".to_string(),
        note: "中文测试 🎉".to_string(),
        color: TagColor::Orange,
        window_title: "창 테스트".to_string(),
        process_name: "test.exe".to_string(),
    };

    assert_eq!(tag.title, "日本語テスト");
    assert_eq!(tag.note, "中文测试 🎉");
    assert_eq!(tag.window_title, "창 테스트");
}

#[test]
fn test_tag_store_large_hwnd() {
    let mut store: TagStore = HashMap::new();
    let max_hwnd = isize::MAX;

    let tag = Tag {
        title: "极值测试".to_string(),
        note: String::new(),
        color: TagColor::Orange,
        window_title: "max".to_string(),
        process_name: "test.exe".to_string(),
    };

    matcher::upsert_tag(&mut store, max_hwnd, tag);
    assert!(matcher::find_tag(&store, max_hwnd).is_some());
    assert!(matcher::find_tag(&store, isize::MIN).is_none());
}
