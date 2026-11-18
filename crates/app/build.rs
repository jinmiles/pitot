//! Delay-loads the system DLLs that only the Pitot window's code imports, so the overlay's process, which never
//! runs that code, does not load them at start. Each loads on the first call into it, in the window's process.

/// Imported only by code the overlay's process never runs. The window's UI framework: `TaskDialogIndirect`, the DWM
/// calls, `u_strlen` (which also brings in `icu.dll`), UI Automation for its accessibility tree (called only when a
/// screen reader or another UI Automation client asks a window), and `timeBeginPeriod` (only while it blocks with a
/// timeout). The window's updates: sockets for its HTTP client, and `BCryptGenRandom` for its TLS (which also brings
/// in `cryptbase.dll`). The overlay's own imports must not be listed here.
const WINDOW_ONLY_DLLS: &[&str] = &[
    "comctl32.dll",
    "dwmapi.dll",
    "icuuc.dll",
    "uiautomationcore.dll",
    "winmm.dll",
    "ws2_32.dll",
    "bcrypt.dll",
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    for dll in WINDOW_ONLY_DLLS {
        println!("cargo::rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    // The MSVC helper that resolves delay-loaded imports on first use.
    println!("cargo::rustc-link-arg-bins=delayimp.lib");
}
