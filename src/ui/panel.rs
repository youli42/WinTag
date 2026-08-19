use eframe::egui;

/// 全局概览面板
pub struct Panel {
    pub search_query: String,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            search_query: String::new(),
        }
    }
}

impl eframe::App for Panel {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("WinTag - 窗口概览");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("搜索：");
                ui.text_edit_singleline(&mut self.search_query);
            });

            ui.separator();

            // 占位：稍后从 TagStore 读取数据填充
            ui.label("暂无已标记的窗口");
            ui.label("按 Ctrl+Shift+N 为当前窗口添加标签");
        });
    }
}