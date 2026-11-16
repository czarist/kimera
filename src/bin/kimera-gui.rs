//! `kimera-gui`: the Kimera window.
#![windows_subsystem = "windows"]

fn main() -> std::process::ExitCode {
    match kimera::gui::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let _ = rfd::MessageDialog::new()
                .set_title("Kimera")
                .set_description(format!("{e:#}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
            std::process::ExitCode::FAILURE
        }
    }
}
