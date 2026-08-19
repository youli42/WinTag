use crate::core::tag::Tag;
use eframe::egui;

/// 悬浮便签浮窗 / 快速标记对话框
#[allow(dead_code)]
pub struct Popup {
    pub title: String,
    pub note: String,
    pub visible: bool,
}

#[allow(dead_code)]
impl Popup {
    pub fn new() -> Self {
        Self {
            title: String::new(),
            note: String::new(),
            visible: false,
        }
    }

    /// 打开快速标记对话框，预填窗口标题
    pub fn open(&mut self, window_title: &str) {
        self.title = window_title.to_string();
        self.note.clear();
        self.visible = true;
    }

    /// 关闭对话框
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// 从当前输入构建 Tag
    pub fn build_tag(&self, window_title: String, process_name: String) -> Tag {
        Tag {
            title: if self.title.is_empty() {
                window_title.clone()
            } else {
                self.title.clone()
            },
            note: self.note.clone(),
            color: crate::core::tag::TagColor::Orange,
            window_title,
            process_name,
        }
    }
}

impl eframe::App for Popup {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("标记当前窗口");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("标题：");
                ui.text_edit_singleline(&mut self.title);
            });

            ui.label("备注：");
            ui.text_edit_multiline(&mut self.note);

            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("确认").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("取消").clicked() {
                    self.title.clear();
                    self.note.clear();
                    self.visible = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}