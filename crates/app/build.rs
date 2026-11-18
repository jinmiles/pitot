//! Delay-loads system DLLs that the overlay's process does not need when it starts: each loads on the first call
//! into it. Most are imported only by code the overlay never runs (the Pitot window's dialogs and updates, GPUI's
//! jump lists); `dwmapi.dll` the overlay calls only once a widget shows.

/// - `dwmapi.dll`: GPUI's display-refresh thread (`DwmFlush`) and the widget windows' square corners, which run
///   only while widgets show; the Pitot window's DWM calls.
/// - `comctl32.dll`: `TaskDialogIndirect`, for GPUI's prompts, which only the Pitot window opens.
/// - `icuuc.dll` (which also brings in `icu.dll`): `u_strlen`, for GPUI's jump lists, which only the Pitot window
///   has.
/// - `uiautomationcore.dll`: GPUI's accessibility tree (AccessKit), called only when a screen reader or another UI
///   Automation client asks a window.
/// - `winmm.dll`: `timeBeginPeriod`, which GPUI calls only while it blocks with a timeout; never in the overlay.
/// - `ws2_32.dll` and `bcrypt.dll` (which also brings in `cryptbase.dll`): sockets and `BCryptGenRandom` for the
///   Pitot window's updates (its HTTP client and TLS).
const DELAY_LOADED_DLLS: &[&str] = &[
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
    for dll in DELAY_LOADED_DLLS {
        println!("cargo::rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    // The MSVC helper that resolves delay-loaded imports on first use.
    println!("cargo::rustc-link-arg-bins=delayimp.lib");
}
