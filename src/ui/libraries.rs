use crate::{
    models::{Document, Library},
    ui::theme,
};

#[derive(Debug)]
pub enum LibraryAction {
    BackToChat,
    Create,
    Select(i64),
    Rename(i64),
    Delete(i64),
    AddDocument,
    CancelIndexing,
    DeleteDocument(i64),
    Reindex(i64),
}

pub fn show(
    ui: &mut egui::Ui,
    libraries: &[Library],
    selected: Option<i64>,
    documents: &[Document],
    new_name: &mut String,
    indexing: bool,
) -> Option<LibraryAction> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        if ui.button("Volver").clicked() {
            action = Some(LibraryAction::BackToChat);
        }
        ui.heading("Bibliotecas y documentos");
    });
    ui.separator();

    ui.strong("Crear biblioteca");
    ui.horizontal(|ui| {
        let button_width = 72.0;
        let field_width =
            (ui.available_width() - button_width - ui.spacing().item_spacing.x - 4.0).max(140.0);
        ui.add_sized(
            [field_width, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(new_name).hint_text("Nombre de la nueva biblioteca"),
        );
        if ui
            .add_sized(
                [button_width, ui.spacing().interact_size.y],
                theme::primary_button("Crear"),
            )
            .clicked()
        {
            action = Some(LibraryAction::Create);
        }
    });
    ui.add_space(10.0);

    ui.columns(2, |columns| {
        theme::surface_frame().show(&mut columns[0], |ui| {
            ui.strong("Bibliotecas");
            ui.add_space(4.0);
            for library in libraries {
                let response = theme::full_width_text_button(
                    ui,
                    28.0,
                    &library.name,
                    selected == Some(library.id),
                );
                if response.clicked() {
                    action = Some(LibraryAction::Select(library.id));
                }
            }
            if let Some(id) = selected {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Renombrar biblioteca").clicked() {
                        action = Some(LibraryAction::Rename(id));
                    }
                    if ui.button("Eliminar").clicked() {
                        action = Some(LibraryAction::Delete(id));
                    }
                });
            }
        });
        theme::surface_frame().show(&mut columns[1], |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Documentos");
                if selected.is_some() && !indexing && ui.button("Agregar archivo").clicked() {
                    action = Some(LibraryAction::AddDocument);
                }
                if indexing && ui.button("Cancelar indexacion").clicked() {
                    action = Some(LibraryAction::CancelIndexing);
                }
            });
            ui.add_space(4.0);
            for document in documents {
                ui.label(&document.name)
                    .on_hover_text(&document.original_path);
                ui.horizontal_wrapped(|ui| {
                    ui.small(&document.status);
                    if ui.small_button("Reindexar").clicked() {
                        action = Some(LibraryAction::Reindex(document.id));
                    }
                    if ui.small_button("Quitar").clicked() {
                        action = Some(LibraryAction::DeleteDocument(document.id));
                    }
                });
                if let Some(error) = &document.error {
                    ui.colored_label(theme::ERROR, "No se pudo indexar el documento")
                        .on_hover_text(error);
                }
                ui.add_space(6.0);
            }
        });
    });
    action
}
