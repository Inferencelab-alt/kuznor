use crate::{
    code::types::{CodeFile, CodeProject},
    ui::theme,
};

#[derive(Debug)]
pub enum CodeAction {
    OpenFile,
    OpenFolder,
    CancelIndexing,
    SelectProject(i64),
    ReindexProject(i64),
    RemoveProject(i64),
    SelectFile(i64),
    QuickPrompt(QuickAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAction {
    Explain,
    FindErrors,
    ReviewLogic,
    SuggestImprovements,
    Refactor,
    Summarize,
}

impl QuickAction {
    pub fn prompt(self) -> &'static str {
        match self {
            Self::Explain => "Explica que hace el archivo seleccionado.",
            Self::FindErrors => "Busca posibles errores logicos en el archivo seleccionado.",
            Self::ReviewLogic => "Revisa la logica del archivo seleccionado y senala riesgos.",
            Self::SuggestImprovements => "Sugiere mejoras concretas para el archivo seleccionado.",
            Self::Refactor => {
                "Propone una version refactorizada del archivo seleccionado, lista para copiar manualmente."
            }
            Self::Summarize => "Resume el archivo seleccionado.",
        }
    }
}

pub fn show_project_panel(
    ui: &mut egui::Ui,
    projects: &[CodeProject],
    active_project: Option<i64>,
    files: &[CodeFile],
    selected_file: Option<i64>,
    status: &str,
    indexing: bool,
) -> Option<CodeAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if ui.add(theme::primary_button("Abrir archivo")).clicked() {
            action = Some(CodeAction::OpenFile);
        }
        if ui.button("Abrir carpeta").clicked() {
            action = Some(CodeAction::OpenFolder);
        }
        if indexing && ui.button("Cancelar").clicked() {
            action = Some(CodeAction::CancelIndexing);
        }
    });
    ui.colored_label(theme::ACCENT_HOVER, "SOLO LECTURA");
    let compact_status = compact_path(status, 54);
    ui.weak(&compact_status).on_hover_text(status);
    if !projects.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.weak("Proyecto:");
            for project in projects {
                if ui
                    .selectable_label(
                        active_project == Some(project.id),
                        format!(
                            "{} [{}]",
                            compact_path(&project.name, 20),
                            project_status_label(&project.status)
                        ),
                    )
                    .on_hover_text(format!("{}\nEstado: {}", project.root_path, project.status))
                    .clicked()
                {
                    action = Some(CodeAction::SelectProject(project.id));
                }
            }
        });
        if let Some(project) = projects
            .iter()
            .find(|project| active_project == Some(project.id))
        {
            ui.strong(format!(
                "Proyecto activo: {}",
                compact_path(&project.name, 28)
            ));
            ui.small(compact_path(&project.root_path, 42))
                .on_hover_text(&project.root_path);
            ui.horizontal_wrapped(|ui| {
                if !matches!(project.status.as_str(), "ready" | "listo")
                    && !indexing
                    && ui.button("Reindexar proyecto").clicked()
                {
                    action = Some(CodeAction::ReindexProject(project.id));
                }
                if ui
                    .add_enabled(!indexing, egui::Button::new("Quitar proyecto"))
                    .on_hover_text("Quita el indice local de Kuznor; no modifica la carpeta.")
                    .clicked()
                {
                    action = Some(CodeAction::RemoveProject(project.id));
                }
            });
        }
    }
    ui.separator();
    if let Some(file) = files.iter().find(|file| selected_file == Some(file.id)) {
        ui.strong(compact_path(&file.relative_path, 38))
            .on_hover_text(&file.relative_path);
        ui.horizontal_wrapped(|ui| {
            for (label, quick_action) in [
                ("Explicar", QuickAction::Explain),
                ("Buscar errores", QuickAction::FindErrors),
                ("Revisar logica", QuickAction::ReviewLogic),
                ("Sugerir mejoras", QuickAction::SuggestImprovements),
                ("Refactorizar", QuickAction::Refactor),
                ("Resumir", QuickAction::Summarize),
            ] {
                if ui.small_button(label).clicked() {
                    action = Some(CodeAction::QuickPrompt(quick_action));
                }
            }
        });
        ui.separator();
    }
    let project_name = projects
        .iter()
        .find(|project| active_project == Some(project.id))
        .map(|project| project.name.as_str())
        .unwrap_or("proyecto activo");
    ui.strong(format!("Archivos de {}", compact_path(project_name, 28)))
        .on_hover_text(project_name);
    let file_list_height = ui.available_height().max(80.0);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), file_list_height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("code_file_tree", active_project))
                .auto_shrink([false, false])
                .max_height(file_list_height)
                .min_scrolled_height(file_list_height)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for file in files {
                        let depth = file.relative_path.matches('/').count();
                        ui.horizontal(|ui| {
                            ui.add_space(depth.min(6) as f32 * 10.0);
                            let full_label = file
                                .relative_path
                                .rsplit('/')
                                .next()
                                .unwrap_or(&file.relative_path);
                            let label = compact_path(full_label, 34);
                            if ui
                                .selectable_label(selected_file == Some(file.id), label)
                                .on_hover_text(&file.relative_path)
                                .clicked()
                            {
                                action = Some(CodeAction::SelectFile(file.id));
                            }
                        });
                        if let Some(error) = &file.error {
                            ui.colored_label(theme::ERROR, "No se pudo indexar")
                                .on_hover_text(error);
                        }
                    }
                });
        },
    );
    action
}

fn compact_path(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let tail = value
        .chars()
        .rev()
        .take(max_chars.saturating_sub(4))
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!(".../{tail}")
}

fn project_status_label(status: &str) -> &str {
    match status {
        "ready" | "listo" => "listo",
        "indexing" | "escaneando" => "indexando",
        "cancelled" => "cancelado",
        "failed" => "fallido",
        "incomplete" => "incompleto",
        _ => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_actions_are_structured_selected_file_requests() {
        for action in [
            QuickAction::Explain,
            QuickAction::FindErrors,
            QuickAction::ReviewLogic,
            QuickAction::SuggestImprovements,
            QuickAction::Refactor,
            QuickAction::Summarize,
        ] {
            assert!(action.prompt().contains("archivo seleccionado"));
        }
    }

    #[test]
    fn long_project_and_file_labels_are_bounded() {
        let label = compact_path(
            "una_ruta_de_proyecto_extremadamente_larga/configuracion_principal.toml",
            24,
        );
        assert!(label.chars().count() <= 24);
        assert!(label.starts_with(".../"));
        assert!(label.ends_with("principal.toml"));
    }
}
