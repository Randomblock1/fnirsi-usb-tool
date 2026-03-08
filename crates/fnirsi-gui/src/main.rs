#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unused_self,
    clippy::needless_pass_by_value
)]

mod app;
mod plots;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("fnirsi=info".parse().unwrap()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "FNIRSI Power Meter",
        options,
        Box::new(|cc| Ok(Box::new(app::FnirsiApp::new(cc)))),
    )
}
