//! `pitot.exe`: the overlay, or with `--window` the Pitot window. Each runs in a process of its own: the overlay
//! starts this executable again with `--window` to open the window, so the overlay's process never starts the
//! window's UI framework.

// Release builds have no console window; the overlay's messages go to its log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        // Other arguments are ignored, as the overlay always did.
        if std::env::args_os()
            .nth(1)
            .is_some_and(|arg| arg == pitot_overlay::platform::WINDOW_FLAG)
        {
            pitot_control::run();
            return ExitCode::SUCCESS;
        }
        pitot_overlay::app::main()
    }
    #[cfg(not(windows))]
    {
        eprintln!("Pitot runs on Windows only");
        ExitCode::FAILURE
    }
}
