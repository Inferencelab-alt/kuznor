use crate::{
    models::{Chat, Library},
    ui::theme,
};

#[derive(Debug)]
pub enum SidebarAction {
    NewChat,
    SelectChat(i64),
    DeleteChat(i64),
    RenameChat(i64),
    SelectLibrary(Option<i64>),
    OpenLibraries,
    OpenSettings,
}

pub fn show(
    ui: &mut egui::Ui,
    chats: &[Chat],
    libraries: &[Library],
    current_chat: Option<i64>,
    active_library: Option<i64>,
    search: &mut String,
    show_libraries: bool,
) -> Option<SidebarAction> {
    let mut action = None;
    ui.add_space(4.0);
    if theme::full_width_button(ui, 34.0, theme::primary_button("+  Nuevo chat")).clicked() {
        action = Some(SidebarAction::NewChat);
    }
    ui.add_space(8.0);
    ui.separator();
    egui::TopBottomPanel::bottom("sidebar_fixed_actions")
        .exact_height(74.0)
        .show_separator_line(true)
        .frame(egui::Frame::new())
        .show_inside(ui, |ui| {
            ui.add_space(6.0);
            if theme::full_width_button(ui, 28.0, egui::Button::new("Bibliotecas y documentos"))
                .clicked()
            {
                action = Some(SidebarAction::OpenLibraries);
            }
            if theme::full_width_button(ui, 28.0, egui::Button::new("Configuracion")).clicked() {
                action = Some(SidebarAction::OpenSettings);
            }
        });
    egui::CentralPanel::default()
        .frame(egui::Frame::new())
        .show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("sidebar_content_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add_space(10.0);
                    section_label(ui, "CHATS");
                    ui.add(
                        egui::TextEdit::singleline(search)
                            .hint_text("Buscar chats...")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(7.0);
                    let query = search.trim().to_lowercase();
                    for chat in chats.iter().filter(|chat| {
                        query.is_empty() || chat.title.to_lowercase().contains(&query)
                    }) {
                        let selected = current_chat == Some(chat.id);
                        let response =
                            theme::full_width_text_button(ui, 28.0, &chat.title, selected);
                        if response.clicked() {
                            action = Some(SidebarAction::SelectChat(chat.id));
                        }
                        response.context_menu(|ui| {
                            if ui.button("Renombrar").clicked() {
                                action = Some(SidebarAction::RenameChat(chat.id));
                                ui.close_menu();
                            }
                            if ui.button("Eliminar").clicked() {
                                action = Some(SidebarAction::DeleteChat(chat.id));
                                ui.close_menu();
                            }
                        });
                    }
                    if show_libraries {
                        ui.add_space(18.0);
                        section_label(ui, "BIBLIOTECAS");
                        ui.add_space(3.0);
                        for library in libraries {
                            if theme::full_width_text_button(
                                ui,
                                28.0,
                                &library.name,
                                active_library == Some(library.id),
                            )
                            .clicked()
                            {
                                action = Some(SidebarAction::SelectLibrary(Some(library.id)));
                            }
                        }
                    }
                });
        });
    action
}

fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(egui::RichText::new(label).small().color(theme::TEXT_MUTED));
}
