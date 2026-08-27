use crate::{config::Settings, ui::theme};

pub fn show(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    chat_connected: bool,
    embedding_connected: bool,
    chat_owned: bool,
    embedding_owned: bool,
    performance_metrics: Option<&str>,
) -> bool {
    let mut save = false;
    let chat_model_available = std::path::Path::new(&settings.chat_model_path).exists();
    let embedding_model_available = std::path::Path::new(&settings.embedding_model_path).exists();
    let initial_setup =
        !chat_connected && (!chat_model_available || settings.llama_command.trim().is_empty());

    if let Some(metrics) = performance_metrics {
        ui.weak(metrics);
    }
    if initial_setup {
        ui.label("Selecciona tus modelos GGUF locales. Kuznor no incluye ni descarga modelos.");
        ui.add_space(8.0);
    }
    theme::surface_frame().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            service_requirement(
                ui,
                "IA principal",
                chat_connected,
                chat_owned,
                chat_model_available,
            );
            ui.separator();
            service_requirement(
                ui,
                "Busqueda semantica",
                embedding_connected,
                embedding_owned,
                embedding_model_available,
            );
        });
    });

    ui.add_space(12.0);
    ui.strong("Modelos locales");
    ui.add_space(6.0);
    ui.label("Modelo principal de IA");
    ui.weak("Usado por General, Documentos y Codigo.");
    path_picker(ui, &mut settings.chat_model_path);
    ui.add_space(10.0);
    ui.label("Modelo de embeddings");
    ui.weak("Usado para buscar informacion en Documentos y Codigo.");
    path_picker(ui, &mut settings.embedding_model_path);

    ui.add_space(14.0);
    egui::Grid::new("settings_response_grid")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label("Temperatura");
            ui.add(egui::Slider::new(&mut settings.temperature, 0.0..=1.5));
            ui.end_row();
            ui.label("Tokens de respuesta");
            ui.add(egui::DragValue::new(&mut settings.max_tokens).range(64..=8192));
            ui.end_row();
        });

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
    ui.strong("Calidad y limitaciones del modelo");
    ui.add_space(4.0);
    ui.weak(
        "Kuznor utiliza el modelo local que tu selecciones. La precision, razonamiento, formato y tendencia a cometer errores dependen en gran medida de ese modelo. Los modelos pequenos pueden responder mas rapido y consumir menos recursos, pero tambien pueden equivocarse con mayor frecuencia en tareas complejas, comparaciones extensas o analisis de varios documentos.",
    );
    ui.add_space(4.0);
    ui.weak(
        "Kuznor intenta mantener el contexto, las fuentes y el aislamiento de datos correctamente, pero no puede garantizar que el modelo genere siempre una respuesta exacta. Para informacion importante, verifica la respuesta con las fuentes mostradas.",
    );
    ui.add_space(4.0);
    ui.weak(
        "Kuznor no busca reemplazar los servicios comerciales de IA. Esta disenado para los casos en los que prefieres o necesitas ejecutar la IA localmente y mantener tus datos bajo tu control.",
    );
    ui.add_space(12.0);
    if ui
        .add(theme::primary_button("Guardar configuracion"))
        .clicked()
    {
        save = true;
    }
    save
}

fn path_picker(ui: &mut egui::Ui, value: &mut String) {
    ui.horizontal(|ui| {
        let button_width = 84.0;
        let field_width =
            (ui.available_width() - button_width - ui.spacing().item_spacing.x - 4.0).max(140.0);
        let visible_path = if value.trim().is_empty() {
            "Ningun modelo seleccionado"
        } else {
            value.as_str()
        };
        let path_response = ui.add_sized(
            [field_width, ui.spacing().interact_size.y],
            egui::Label::new(visible_path)
                .truncate()
                .sense(egui::Sense::hover()),
        );
        if !value.trim().is_empty() {
            path_response.on_hover_text(value.as_str());
        }
        if ui
            .add_sized(
                [button_width, ui.spacing().interact_size.y],
                egui::Button::new("Elegir..."),
            )
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Modelo GGUF", &["gguf"])
                .pick_file()
            {
                *value = path.to_string_lossy().into_owned();
            }
        }
    });
}

fn service_requirement(
    ui: &mut egui::Ui,
    name: &str,
    connected: bool,
    owned: bool,
    model_available: bool,
) {
    let (color, description) = if connected && owned {
        (theme::SUCCESS, "servidor administrado conectado")
    } else if connected {
        (theme::SUCCESS, "servidor local conectado")
    } else if model_available {
        (theme::ACCENT_HOVER, "modelo local configurado")
    } else {
        (theme::ERROR, "sin servidor accesible ni modelo local")
    };
    ui.colored_label(color, format!("{name}: {description}"));
}
