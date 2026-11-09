// Release builds have no console window; messages go to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        pitot_overlay::log::init();
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
