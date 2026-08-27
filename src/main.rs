mod ai;
mod app;
mod code;
mod config;
mod db;
mod documents;
mod models;
mod performance;
mod rag;
mod ui;

const KUZNOR_LOGO_PNG: &[u8] = include_bytes!("../assets/kuznor.png");

fn kuznor_icon_data() -> egui::IconData {
    eframe::icon_data::from_png_bytes(KUZNOR_LOGO_PNG)
        .expect("assets/kuznor.png debe ser un PNG valido")
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kuznor")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([860.0, 560.0])
            .with_icon(kuznor_icon_data()),
        ..Default::default()
    };

    eframe::run_native(
        "Kuznor",
        options,
        Box::new(|cc| {
            app::KuznorApp::new(cc)
                .map(|app| Box::new(app) as Box<dyn eframe::App>)
                .map_err(|err| {
                    let wrapped = err.context("No se pudo iniciar la aplicacion");
                    Box::<dyn std::error::Error + Send + Sync>::from(wrapped)
                })
        }),
    )
}
