use egui::{Color32, FontFamily, FontId, TextStyle};

pub const BACKGROUND: Color32 = Color32::from_rgb(0x11, 0x13, 0x15);
pub const PANEL: Color32 = Color32::from_rgb(0x18, 0x1b, 0x1f);
pub const SURFACE: Color32 = Color32::from_rgb(0x20, 0x24, 0x2a);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x29, 0x2e, 0x35);
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x36);
pub const TEXT: Color32 = Color32::from_rgb(0xe7, 0xe7, 0xe7);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9c, 0xa3, 0xaf);
pub const ACCENT: Color32 = Color32::from_rgb(0xb8, 0x5c, 0x38);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xd0, 0x6b, 0x43);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0x3d, 0x29, 0x22);
pub const SUCCESS: Color32 = Color32::from_rgb(0x4f, 0xb0, 0x72);
pub const WARNING: Color32 = Color32::from_rgb(0xd1, 0xa1, 0x4d);
pub const ERROR: Color32 = Color32::from_rgb(0xd0, 0x5a, 0x62);
pub const CODE_BACKGROUND: Color32 = Color32::from_rgb(0x0d, 0x0f, 0x12);

pub const CORNER_RADIUS: u8 = 6;
pub const CONTENT_PADDING: i8 = 14;
pub const SIDEBAR_WIDTH: f32 = 232.0;
pub const HOVER_GUTTER: f32 = 3.0;

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = CODE_BACKGROUND;
    visuals.faint_bg_color = SURFACE;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke.color = TEXT;
    visuals.hyperlink_color = ACCENT_HOVER;
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke.color = BORDER;
    visuals.widgets.inactive.bg_fill = SURFACE;
    visuals.widgets.inactive.bg_stroke.color = BORDER;
    visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
    visuals.widgets.hovered.bg_stroke.color = ACCENT_HOVER;
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke.color = ACCENT_HOVER;
    visuals.widgets.open.bg_fill = ACCENT_SOFT;
    visuals.widgets.open.bg_stroke.color = ACCENT;
    visuals.window_corner_radius = egui::CornerRadius::same(CORNER_RADIUS);
    visuals.menu_corner_radius = egui::CornerRadius::same(CORNER_RADIUS);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.scroll.bar_width = 9.0;
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(13.0, FontFamily::Monospace),
    );
    ctx.set_style(style);
}

pub fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::same(CONTENT_PADDING))
}

pub fn surface_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(CORNER_RADIUS)
        .inner_margin(egui::Margin::same(CONTENT_PADDING))
}

pub fn primary_button(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(text.into())
        .fill(ACCENT)
        .stroke(egui::Stroke::new(1.0, ACCENT_HOVER))
}

pub fn full_width_button(
    ui: &mut egui::Ui,
    height: f32,
    button: egui::Button<'_>,
) -> egui::Response {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(HOVER_GUTTER as i8, 0))
        .show(ui, |ui| {
            ui.add_sized([ui.available_width().max(40.0), height], button)
        })
        .inner
}

pub fn full_width_text_button(
    ui: &mut egui::Ui,
    height: f32,
    text: &str,
    selected: bool,
) -> egui::Response {
    let response = full_width_button(
        ui,
        height,
        egui::Button::new(text).selected(selected).truncate(),
    );
    response.on_hover_text(text)
}
