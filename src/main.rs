mod core;
mod hotkey;
mod sys;
mod ui;

use core::tag::{Tag, TagColor, TagStore};
use std::sync::{Arc, Mutex};
use std::thread;
use winit::platform::windows::EventLoopBuilderExtWindows;
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, TranslateMessage, DispatchMessageW, MSG,
};

fn main() -> anyhow::Result<()> {
    println!("WinTag 启动中...");

    // 注册全局热键
    hotkey::register_all()?;
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
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };

        if ret.0 == 0 {
            break;
        }

        if ret.0 == -1 {
            anyhow::bail!("GetMessage 错误");
        }

        // 检查热键消息
        if let Some(hotkey) = hotkey::from_message(msg.message, msg.wParam.0, msg.lParam.0) {
            match hotkey {
                hotkey::Hotkey::QuickTag => {
                    handle_quick_tag(Arc::clone(&store_clone));
                }
                hotkey::Hotkey::TogglePanel => {
                    handle_toggle_panel(Arc::clone(&store_clone));
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

/// 处理快速标记热键
fn handle_quick_tag(store: Arc<Mutex<TagStore>>) {
    match sys::window::get_foreground_window_info() {
        Ok(info) => {
            // 检查是否已有标签
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
                    Box::new(|_cc| {
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
            Box::new(|_cc| {
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
                                ui.label(format!("📌 {}", tag.window_title));
                                ui.label(format!("({})", tag.process_name));
                            });

                            if ui.button("跳转到此窗口").clicked() {
                                // SAFETY: 激活目标窗口
                                unsafe {
                                    let hwnd =
                                        windows::Win32::Foundation::HWND(*hwnd as *mut std::ffi::c_void);
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