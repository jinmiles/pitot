// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    pitot_control::app::run();
    #[cfg(not(windows))]
    eprintln!("Pitot runs on Windows only");
}
