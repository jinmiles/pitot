//! Win32 platform layer: DPI awareness, monitors, overlay windows, the message loop, timers, the tray icon, and
//! the single-instance lock.
//!
//! All Win32 `unsafe` for windowing lives here; callers use the safe types below.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{
    COLORREF, CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, POINT, RECT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateMutexW, CreateWaitableTimerExW, SetWaitableTimer,
    TIMER_ALL_ACCESS,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, HTTRANSPARENT, IDI_APPLICATION, LWA_ALPHA, LoadIconW,
    MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
    PM_REMOVE, PeekMessageW, PostQuitMessage, QS_ALLINPUT, RegisterClassExW, SW_SHOWNOACTIVATE,
    SetForegroundWindow, SetLayeredWindowAttributes, ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE,
    WM_CONTEXTMENU, WM_LBUTTONUP, WM_NCHITTEST, WM_QUIT, WM_RBUTTONUP, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// A Win32 failure, with the call that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformError {
    pub call: &'static str,
    pub code: i32,
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed with HRESULT {:#010x}", self.call, self.code)
    }
}

impl std::error::Error for PlatformError {}

fn error(call: &'static str, e: &windows::core::Error) -> PlatformError {
    PlatformError {
        call,
        code: e.code().0,
    }
}

fn last_error(call: &'static str) -> PlatformError {
    error(call, &windows::core::Error::from_thread())
}

/// Opts the process into per-monitor DPI awareness, so window and monitor coordinates are physical pixels.
/// Local wall-clock time, for log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

pub fn local_time() -> LocalTime {
    // SAFETY: GetLocalTime only fills the SYSTEMTIME it returns and cannot fail.
    let t = unsafe { GetLocalTime() };
    LocalTime {
        year: t.wYear,
        month: t.wMonth,
        day: t.wDay,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
        millis: t.wMilliseconds,
    }
}

pub fn enable_dpi_awareness() {
    // SAFETY: plain process-wide setting with a predefined context value. It fails harmlessly when already
    // set, for example by a manifest.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// A rectangle in physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A display, in enumeration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub rect: ScreenRect,
    pub primary: bool,
    /// Effective DPI (96 at 100% scaling).
    pub dpi: u32,
}

/// Lists the connected monitors.
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> windows::core::BOOL {
        // SAFETY: `data` is the `&mut Vec<Monitor>` passed to EnumDisplayMonitors below, alive for the call.
        let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` is a MONITORINFOEXW with cbSize set, as GetMonitorInfoW requires.
        if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo as *mut MONITORINFO) }.as_bool()
        {
            let r = info.monitorInfo.rcMonitor;
            let (mut dpi_x, mut dpi_y) = (96, 96);
            // SAFETY: valid monitor handle from the enumeration and writable out parameters.
            let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
            out.push(Monitor {
                rect: ScreenRect {
                    x: r.left,
                    y: r.top,
                    width: r.right - r.left,
                    height: r.bottom - r.top,
                },
                primary: info.monitorInfo.dwFlags & 1 != 0,
                dpi: dpi_x,
            });
        }
        true.into()
    }
    let mut out: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only runs during this call and receives a pointer to `out`, which outlives it.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut out as *mut _ as isize),
        )
    };
    out
}

const TRAY_MESSAGE: u32 = WM_APP + 1;
const CMD_RELOAD: u8 = 1;
const CMD_QUIT: u8 = 2;
const CMD_HIDE: u8 = 3;

/// Tray menu choice waiting to be handled by the app loop: one of the `CMD_` values, or 0.
static PENDING: AtomicU8 = AtomicU8::new(0);
/// Whether the menu shows *Hide widgets* checked; set by the app through [`Tray::set_hidden`].
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// A command chosen from the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Reload,
    /// Hide the widgets if they are shown, show them if they are hidden.
    ToggleHidden,
}

/// Takes the pending tray command, if any. Quit ends the message loop instead.
pub fn take_tray_command() -> Option<TrayCommand> {
    match PENDING.swap(0, Ordering::Relaxed) {
        CMD_RELOAD => Some(TrayCommand::Reload),
        CMD_HIDE => Some(TrayCommand::ToggleHidden),
        _ => None,
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Layered and transparent windows get no mouse input at all; this only covers windows of this thread.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        // Closing any window quits like the tray's Quit, so tools that close apps the usual way end the overlay
        // cleanly instead of leaving it running without that window.
        WM_CLOSE => {
            // SAFETY: posts WM_QUIT to this thread's own message queue.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // Either button opens the menu: a left click is what most people try first.
        TRAY_MESSAGE
            if matches!(
                lparam.0 as u32,
                WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU
            ) =>
        {
            show_tray_menu(hwnd);
            LRESULT(0)
        }
        // SAFETY: forwards unchanged arguments for default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn show_tray_menu(hwnd: HWND) {
    // SAFETY: the menu is created, shown modally for `hwnd` (a window of this thread), and destroyed here.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let hide = if HIDDEN.load(Ordering::Relaxed) {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = AppendMenuW(menu, hide, usize::from(CMD_HIDE), w!("Hide widgets"));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            usize::from(CMD_RELOAD),
            w!("Reload configuration"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, usize::from(CMD_QUIT), w!("Quit Pitot"));
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON;
        // With TPM_RETURNCMD the result is the chosen item's id, or 0.
        let chosen = TrackPopupMenu(menu, flags, cursor.x, cursor.y, None, hwnd, None).0;
        let _ = DestroyMenu(menu);
        match u8::try_from(chosen) {
            Ok(CMD_QUIT) => PostQuitMessage(0),
            Ok(command @ (CMD_RELOAD | CMD_HIDE)) => PENDING.store(command, Ordering::Relaxed),
            _ => {}
        }
    }
}

fn instance() -> Result<HINSTANCE, PlatformError> {
    // SAFETY: querying the handle of the running executable.
    unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map(Into::into)
        .map_err(|e| error("GetModuleHandleW", &e))
}

fn class() -> Result<PCWSTR, PlatformError> {
    static REGISTERED: OnceLock<Result<(), PlatformError>> = OnceLock::new();
    let name = w!("PitotOverlay");
    REGISTERED
        .get_or_init(|| {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance()?,
                lpszClassName: name,
                ..Default::default()
            };
            // SAFETY: the class structure is fully initialized and its strings are static.
            if unsafe { RegisterClassExW(&class) } == 0 {
                return Err(last_error("RegisterClassExW"));
            }
            Ok(())
        })
        .clone()?;
    Ok(name)
}

/// Extended styles of the overlay windows. Only `WS_EX_LAYERED` together with `WS_EX_TRANSPARENT` takes a
/// window out of mouse hit testing, so clicks reach the windows below, also those of other processes.
/// `WS_EX_NOREDIRECTIONBITMAP` leaves the content to DirectComposition.
const OVERLAY_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(
    WS_EX_LAYERED.0
        | WS_EX_TRANSPARENT.0
        | WS_EX_NOREDIRECTIONBITMAP.0
        | WS_EX_TOPMOST.0
        | WS_EX_NOACTIVATE.0
        | WS_EX_TOOLWINDOW.0,
);

/// A borderless, transparent, click-through, topmost window that never takes focus.
#[derive(Debug)]
pub struct OverlayWindow {
    hwnd: HWND,
}

impl OverlayWindow {
    /// Creates a window covering `rect` and shows it without activating it.
    pub fn create(rect: ScreenRect) -> Result<Self, PlatformError> {
        let style = OVERLAY_STYLE;
        // SAFETY: the class is registered and all strings are static; no creation parameter is passed.
        let hwnd = unsafe {
            CreateWindowExW(
                style,
                class()?,
                w!("Pitot"),
                WS_POPUP,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                None,
                None,
                Some(instance()?),
                None,
            )
        }
        .map_err(|e| error("CreateWindowExW", &e))?;
        // A layered window stays invisible until its attributes are set. Full opacity leaves the composition
        // content as drawn.
        // SAFETY: `hwnd` was just created by this thread with WS_EX_LAYERED.
        if let Err(e) = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) } {
            // SAFETY: the window was created above and is destroyed once, here.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(error("SetLayeredWindowAttributes", &e));
        }
        // SAFETY: `hwnd` was just created by this thread.
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        Ok(Self { hwnd })
    }

    pub(crate) fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // SAFETY: the window belongs to this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// The notification-area icon with its menu (Hide widgets, Reload configuration, Quit), owned by a hidden
/// window.
#[derive(Debug)]
pub struct Tray {
    window: OverlayWindow,
}

impl Tray {
    pub fn create() -> Result<Self, PlatformError> {
        let window = OverlayWindow::create(ScreenRect::default())?;
        let mut data = tray_data(window.hwnd);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = TRAY_MESSAGE;
        // SAFETY: loads a stock system icon.
        data.hIcon =
            unsafe { LoadIconW(None, IDI_APPLICATION) }.map_err(|e| error("LoadIconW", &e))?;
        for (slot, unit) in data.szTip.iter_mut().zip("Pitot".encode_utf16()) {
            *slot = unit;
        }
        // SAFETY: `data` is a fully initialized NOTIFYICONDATAW for a window of this thread.
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return Err(last_error("Shell_NotifyIconW"));
        }
        Ok(Self { window })
    }

    /// Shows *Hide widgets* checked or not the next time the menu opens.
    pub fn set_hidden(&self, hidden: bool) {
        HIDDEN.store(hidden, Ordering::Relaxed);
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let data = tray_data(self.window.hwnd);
        // SAFETY: removes the icon added in `create` for the same window and id.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }
}

/// A high-resolution waitable timer for the render schedule. Never uses `timeBeginPeriod`.
#[derive(Debug)]
pub struct Timer {
    handle: HANDLE,
}

impl Timer {
    pub fn new() -> Result<Self, PlatformError> {
        // SAFETY: creates an unnamed timer with default security.
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .map_err(|e| error("CreateWaitableTimerExW", &e))?;
        Ok(Self { handle })
    }

    /// Fires once after `delay`.
    pub fn set(&self, delay: Duration) -> Result<(), PlatformError> {
        // Negative due times are relative, in 100 ns units.
        let due = -i64::try_from(delay.as_nanos() / 100)
            .unwrap_or(i64::MAX)
            .max(1);
        // SAFETY: `handle` is a timer owned by `self`; no completion routine.
        unsafe { SetWaitableTimer(self.handle, &due, 0, None, None, false) }
            .map_err(|e| error("SetWaitableTimer", &e))
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: closes the timer handle owned by `self`, once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Name of the mutex that marks a running overlay in this Windows session.
const INSTANCE_MUTEX: PCWSTR = w!("Local\\PitotOverlay");

/// Held for the life of the process so that a second overlay can tell one is already running. The OS releases
/// it when the process exits, also after a crash.
#[derive(Debug)]
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// Takes the overlay's instance lock. `Ok(None)` means another overlay in this session holds it.
    pub fn acquire() -> Result<Option<Self>, PlatformError> {
        Self::acquire_named(INSTANCE_MUTEX)
    }

    fn acquire_named(name: PCWSTR) -> Result<Option<Self>, PlatformError> {
        // SAFETY: `name` is a static NUL-terminated wide string; default security. The mutex is never owned:
        // only its existence matters.
        let handle =
            unsafe { CreateMutexW(None, false, name) }.map_err(|e| error("CreateMutexW", &e))?;
        // SAFETY: reads this thread's last error, which CreateMutexW just set.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: closes the handle opened above, once.
            let _ = unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Self { handle }))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: closes the mutex handle owned by `self`, once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// A change notification on a directory's file names and write times, for reloading configuration without
/// polling.
#[derive(Debug)]
pub struct DirectoryWatch {
    handle: HANDLE,
}

impl DirectoryWatch {
    pub fn new(dir: &std::path::Path) -> Result<Self, PlatformError> {
        let path = windows::core::HSTRING::from(dir.as_os_str());
        // SAFETY: `path` is a NUL-terminated wide string that outlives the call.
        let handle = unsafe {
            FindFirstChangeNotificationW(
                &path,
                false,
                FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
            )
        }
        .map_err(|e| error("FindFirstChangeNotificationW", &e))?;
        Ok(Self { handle })
    }

    /// Re-arms the notification after it was signaled.
    fn rearm(&self) {
        // SAFETY: `handle` is a change notification owned by `self`.
        let _ = unsafe { FindNextChangeNotification(self.handle) };
    }
}

impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        // SAFETY: closes the notification handle owned by `self`, once.
        let _ = unsafe { FindCloseChangeNotification(self.handle) };
    }
}

/// What ended a [`wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Timer,
    /// The watched directory changed.
    Changed,
    Messages,
    Timeout,
}

/// Sleeps until the timer fires, the watched directory changes, a window message arrives, or `timeout`
/// passes.
pub fn wait(timer: &Timer, watch: Option<&DirectoryWatch>, timeout: Duration) -> Wake {
    let millis = u32::try_from(timeout.as_millis())
        .unwrap_or(u32::MAX - 1)
        .min(u32::MAX - 1);
    let both = [timer.handle, watch.map_or(timer.handle, |w| w.handle)];
    let handles = if watch.is_some() {
        &both[..]
    } else {
        &both[..1]
    };
    // SAFETY: waits on handles owned by `timer` and `watch`, and on this thread's message queue.
    let result = unsafe {
        MsgWaitForMultipleObjectsEx(Some(handles), millis, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
    };
    match result {
        WAIT_OBJECT_0 => Wake::Timer,
        WAIT_TIMEOUT => Wake::Timeout,
        r if watch.is_some() && r.0 == WAIT_OBJECT_0.0 + 1 => {
            if let Some(watch) = watch {
                watch.rearm();
            }
            Wake::Changed
        }
        _ => Wake::Messages,
    }
}

/// Dispatches queued window messages. Returns `false` once Quit was chosen.
pub fn pump_messages() -> bool {
    let mut msg = MSG::default();
    // SAFETY: standard message loop over this thread's queue with a valid MSG buffer.
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_choices_reach_the_app_once() {
        PENDING.store(CMD_HIDE, Ordering::Relaxed);
        assert_eq!(take_tray_command(), Some(TrayCommand::ToggleHidden));
        assert_eq!(take_tray_command(), None, "taken once");
        PENDING.store(CMD_RELOAD, Ordering::Relaxed);
        assert_eq!(take_tray_command(), Some(TrayCommand::Reload));
    }

    #[test]
    fn second_instance_is_refused_until_the_first_exits() {
        let name = w!("Local\\PitotOverlayTest");
        let first = SingleInstance::acquire_named(name)
            .expect("create mutex")
            .expect("first instance takes the lock");
        assert!(
            SingleInstance::acquire_named(name)
                .expect("open mutex")
                .is_none()
        );
        drop(first);
        assert!(
            SingleInstance::acquire_named(name)
                .expect("create mutex")
                .is_some(),
            "released when the first instance exits"
        );
    }
}
