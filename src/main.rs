mod core;
mod hotkey;
mod sys;
mod ui;

use core::tag::{Tag, TagColor, TagStore};
use std::sync::{Arc, Mutex};
use std::thread;
use winit::platform::windows::EventLoopBuilderExtWindows;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetMessageW, RegisterClassW, TranslateMessage,
    DispatchMessageW, CS_HREDRAW, CS_VREDRAW, MSG, WINDOW_EX_STYLE, WM_HOTKEY,
    WNDCLASSW, WS_OVERLAPPED,
};

fn main() -> anyhow::Result<()> {
    println!("WinTag 启动中...");

    // 创建隐藏窗口（用于热键消息接收）
    let hwnd = create_hidden_window()?;

    // 注册全局热键到隐藏窗口
    hotkey::register_all(hwnd)?;
    println!("热键已注册：");
    println!("  Ctrl+Shift+N — 快速标记当前窗口");
    println!("  Ctrl+Shift+M — 打开概览面板");

    // 共享的标签存储
    let tag_store: Arc<Mutex<TagStore>> = Arc::new(Mutex::new(TagStore::new()));

    // 运行 Windows 消息循环
    let store_clone = Arc::clone(&tag_store);
    let mut msg = MSG::default();

    loop {
        // SAFETY: GetMessageW 是标准 Windows 消息循环 API
        let ret = unsafe { GetMessageW(&mut msg, hwnd, 0, 0) };

        if ret.0 == 0 {
            break;
        }

        if ret.0 == -1 {
            anyhow::bail!("GetMessage 错误");
        }

        // 检查热键消息
        if msg.message == WM_HOTKEY {
            let hotkey = hotkey::from_message(msg.message, msg.wParam.0, msg.lParam.0);
            if let Some(hk) = hotkey {
                match hk {
                    hotkey::Hotkey::QuickTag => {
                        handle_quick_tag(Arc::clone(&store_clone));
                    }
                    hotkey::Hotkey::TogglePanel => {
                        handle_toggle_panel(Arc::clone(&store_clone));
                    }
                }
            }
            continue;
        }

        // SAFETY: 标准消息翻译和分发
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }

    Ok(())
}

/// 创建隐藏窗口用于接收全局热键消息
fn create_hidden_window() -> anyhow::Result<HWND> {
    let class_name = widestring("WinTagHiddenWnd");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(hidden_wndproc),
        hInstance: HINSTANCE::default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    // SAFETY: 注册自定义窗口类
    unsafe {
        let _ = RegisterClassW(&wc);
    }

    // SAFETY: 创建隐藏窗口，参数合法
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            windows::core::w!("WinTag"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
    }?;

    Ok(hwnd)
}

/// 加载中文字体到 egui context
fn load_chinese_fonts(ctx: &egui::Context) {
    let font_paths = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyhbd.ttc",
        "C:\\Windows\\Fonts\\simsun.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
    ];

    let mut fonts = egui::FontDefinitions::default();
    let mut loaded = false;

    for path in &font_paths {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(
                "system-cjk".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(data)),
            );
            loaded = true;
            break;
        }
    }

    if loaded {
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "system-cjk".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "system-cjk".to_owned());
        ctx.set_fonts(fonts);
    }
}

fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 隐藏窗口的窗口过程
extern "system" fn hidden_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: 默认窗口过程
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 处理快速标记热键
fn handle_quick_tag(store: Arc<Mutex<TagStore>>) {
    match sys::window::get_foreground_window_info() {
        Ok(info) => {
            let existing = {
                let store = store.lock().unwrap();
                store.get(&info.hwnd).cloned()
            };

            if let Some(tag) = existing {
                println!(
                    "窗口已有标签：{} ({}), 备注：{}",
                    tag.title, info.process_name, tag.note
                );
            }

            let store_clone = Arc::clone(&store);
            let window_title = info.title.clone();
            let process_name = info.process_name.clone();
            let hwnd = info.hwnd;

            thread::spawn(move || {
                let native_options = eframe::NativeOptions {
                    viewport: egui::ViewportBuilder::default()
                        .with_inner_size([400.0, 300.0])
                        .with_title("标记窗口"),
                    event_loop_builder: Some(Box::new(|builder| {
                        builder.with_any_thread(true);
                    })),
                    ..Default::default()
                };

                let mut popup = ui::popup::Popup::new();
                popup.open(&window_title);

                let _ = eframe::run_native(
                    "wintag-quick-tag",
                    native_options,
                    Box::new(|cc| {
                        load_chinese_fonts(&cc.egui_ctx);
                        Ok(Box::new(QuickTagApp {
                            popup,
                            store: store_clone,
                            hwnd,
                            window_title: window_title.clone(),
                            process_name: process_name.clone(),
                            confirmed: false,
                        }))
                    }),
                );
            });
        }
        Err(e) => {
            eprintln!("获取窗口信息失败: {}", e);
        }
    }
}

/// 处理面板切换热键
fn handle_toggle_panel(store: Arc<Mutex<TagStore>>) {
    let store_clone = Arc::clone(&store);

    thread::spawn(move || {
        let native_options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([500.0, 400.0])
                .with_title("WinTag - 概览面板"),
            event_loop_builder: Some(Box::new(|builder| {
                builder.with_any_thread(true);
            })),
            ..Default::default()
        };

        let _ = eframe::run_native(
            "wintag-panel",
            native_options,
            Box::new(|cc| {
                load_chinese_fonts(&cc.egui_ctx);
                Ok(Box::new(PanelApp {
                    panel: ui::panel::Panel::new(),
                    store: store_clone,
                }))
            }),
        );
    });
}

/// 快速标记窗口的 egui App
struct QuickTagApp {
    popup: ui::popup::Popup,
    store: Arc<Mutex<TagStore>>,
    hwnd: isize,
    window_title: String,
    process_name: String,
    confirmed: bool,
}

impl eframe::App for QuickTagApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.popup.visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("标记当前窗口");
            ui.label(format!("窗口：{}", self.window_title));
            ui.label(format!("进程：{}", self.process_name));
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("标题：");
                ui.text_edit_singleline(&mut self.popup.title);
            });

            ui.label("备注：");
            ui.text_edit_multiline(&mut self.popup.note);

            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("确认").clicked() {
                    let tag = Tag {
                        title: if self.popup.title.is_empty() {
                            self.window_title.clone()
                        } else {
                            self.popup.title.clone()
                        },
                        note: self.popup.note.clone(),
                        color: TagColor::Orange,
                        window_title: self.window_title.clone(),
                        process_name: self.process_name.clone(),
                    };

                    {
                        let mut store = self.store.lock().unwrap();
                        crate::core::matcher::upsert_tag(&mut store, self.hwnd, tag);
                    }

                    println!("已标记窗口：{}", self.window_title);
                    self.popup.visible = false;
                    self.confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("取消").clicked() {
                    self.popup.visible = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}

/// 概览面板的 egui App
struct PanelApp {
    panel: ui::panel::Panel,
    store: Arc<Mutex<TagStore>>,
}

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("WinTag - 窗口概览");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("搜索：");
                ui.text_edit_singleline(&mut self.panel.search_query);
            });

            ui.separator();

            let store = self.store.lock().unwrap();
            if store.is_empty() {
                ui.label("暂无已标记的窗口");
                ui.label("按 Ctrl+Shift+N 为当前窗口添加标签");
            } else {
                let query = self.panel.search_query.to_lowercase();
                let mut entries: Vec<_> = store
                    .iter()
                    .filter(|(_, tag)| {
                        query.is_empty()
                            || tag.title.to_lowercase().contains(&query)
                            || tag.note.to_lowercase().contains(&query)
                            || tag.window_title.to_lowercase().contains(&query)
                    })
                    .collect();
                entries.sort_by(|a, b| a.1.title.cmp(&b.1.title));

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (hwnd, tag) in entries {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.strong(&tag.title);
                            if !tag.note.is_empty() {
                                ui.label(&tag.note);
                            }
                            ui.horizontal(|ui| {
                                ui.label(format!("窗口 {}", tag.window_title));
                                ui.label(format!("({})", tag.process_name));
                            });

                            if ui.button("跳转到此窗口").clicked() {
                                // SAFETY: 激活目标窗口
                                unsafe {
                                    let hwnd = HWND(*hwnd as *mut std::ffi::c_void);
                                    let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                                }
                            }
                        });
                    }
                });
            }
        });
    }
}