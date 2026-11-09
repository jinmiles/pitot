use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    match pitot_overlay::app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("Pitot runs on Windows only");
        ExitCode::FAILURE
    }
}
