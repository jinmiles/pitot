// Release builds have no console window; messages go to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        // Checked before logging starts, so a second launch leaves the running overlay's log alone.
        let instance = pitot_overlay::platform::SingleInstance::acquire();
        if matches!(instance, Ok(None)) {
            return ExitCode::SUCCESS;
        }
        pitot_overlay::log::init();
        if let Err(e) = &instance {
            tracing::warn!("cannot check for a running overlay: {e}");
        }
        match pitot_overlay::app::run() {
            Ok(()) => {
                tracing::info!("exiting");
                ExitCode::SUCCESS
            }
            Err(e) => {
                tracing::error!("stopped: {e}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("Pitot runs on Windows only");
        ExitCode::FAILURE
    }
}
