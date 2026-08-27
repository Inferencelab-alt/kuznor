pub mod chat;
pub mod code;
pub mod libraries;
pub mod settings;
pub mod sidebar;
pub mod theme;

pub fn apply_theme(ctx: &egui::Context) {
    theme::apply(ctx);
}

pub fn status_dot(ui: &mut egui::Ui, color: egui::Color32, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(label);
    })
    .response
}
