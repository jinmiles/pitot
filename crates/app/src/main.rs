//! `pitot.exe`: the overlay, or with `--window` the Pitot window. Each runs in a process of its own: the overlay
//! starts this executable again with `--window` to open the window, so the overlay's process never starts the
//! window's UI framework.

// Release builds have no console window; the overlay's messages go to its log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        let launch = installer_hooks();
        // Other arguments are ignored, as the overlay always did.
        if std::env::args_os()
            .nth(1)
            .is_some_and(|arg| arg == pitot_overlay::platform::WINDOW_FLAG)
        {
            pitot_control::run();
            return ExitCode::SUCCESS;
        }
        pitot_overlay::app::main(launch)
    }
    #[cfg(not(windows))]
    {
        eprintln!("Pitot runs on Windows only");
        ExitCode::FAILURE
    }
}

/// The installer's start-up hook, first in every mode. The installer, its updater, and its uninstaller start this
/// executable with arguments of their own around each step; for those the hook ends the process at once. Updates
/// are installed only from the Pitot window (`pitot_control`), never on a start, so an overlay is never stopped by
/// one. Run from anywhere but an installation (`target\release`, a copied file), it finds nothing and returns.
#[cfg(windows)]
fn installer_hooks() -> pitot_overlay::app::Launch {
    let mut launch = pitot_overlay::app::Launch::default();
    velopack::VelopackApp::build()
        .set_auto_apply_on_startup(false)
        .on_restarted(|_| launch.after_update = true)
        .run();
    launch
}
