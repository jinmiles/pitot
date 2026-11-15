//! Win32 platform layer: DPI awareness, monitors, overlay windows, the message loop, timers, the tray icon, the
//! single-instance lock, and the [`link`] to the Pitot window.
//!
//! All Win32 `unsafe` for windowing lives here; callers use the safe types below.

mod link;

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use link::{
    ControlEvent, ControlLink, OverlayCommand, WINDOW_FLAG, activate_control, notify_control,
    open_control, overlay_running, send_to_overlay, start_overlay,
};
use link::{LINK_MESSAGE, TRAY_CLASS, register_class};

use windows::Win32::Foundation::{
    COLORREF, CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, POINT, RECT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Com::CoTaskMemFree;
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
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::{
    FOLDERID_Documents, KF_FLAG_DEFAULT, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW, SHGetKnownFolderPath, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HTTRANSPARENT,
    HWND_TOPMOST, IDC_SIZEALL, IDI_APPLICATION, LWA_ALPHA, LoadCursorW, LoadIconW, MA_NOACTIVATE,
    MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
    PM_REMOVE, PeekMessageW, PostQuitMessage, QS_ALLINPUT, RegisterClassExW, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
    SetLayeredWindowAttributes, SetMenuDefaultItem, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
    WINDOW_EX_STYLE, WM_APP, WM_CAPTURECHANGED, WM_CLOSE, WM_CONTEXTMENU, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_QUIT,
    WM_RBUTTONUP, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
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

/// The user's Documents folder, where it is now (it may be redirected, for example into OneDrive).
pub fn documents_dir() -> Option<std::path::PathBuf> {
    // SAFETY: SHGetKnownFolderPath returns a NUL-terminated string allocated with CoTaskMemAlloc, which is read
    // once and then freed exactly once.
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string();
        CoTaskMemFree(Some(path.as_ptr().cast()));
        text.ok().map(std::path::PathBuf::from)
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
const CMD_RELOAD: usize = 1;
const CMD_QUIT: usize = 2;
const CMD_HIDE: usize = 3;
const CMD_MOVE: usize = 4;
const CMD_OPEN: usize = 5;

/// Whether the menu shows *Hide widgets* checked; set by the app through [`Tray::set_hidden`].
static HIDDEN: AtomicBool = AtomicBool::new(false);
/// Whether the menu shows *Move widgets* checked; set by the app through [`Tray::set_moving`].
static MOVING: AtomicBool = AtomicBool::new(false);

/// A command from the tray menu or the Pitot window, for the app loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Reload,
    /// Hide the widgets, or show them again.
    SetHidden(bool),
    /// Start or end moving widgets with the mouse.
    SetMoving(bool),
    /// The Pitot window's background opacity slider, in percent: moved, or saved into `config.toml`.
    BackgroundOpacity {
        percent: u8,
        saved: bool,
    },
}

thread_local! {
    // The tray window belongs to the app thread, so its commands queue there.
    static COMMANDS: RefCell<Vec<TrayCommand>> = const { RefCell::new(Vec::new()) };
}

fn push_command(command: TrayCommand) {
    COMMANDS.with(|commands| commands.borrow_mut().push(command));
}

/// Takes the oldest command waiting, if any. Quit ends the message loop instead.
pub fn take_tray_command() -> Option<TrayCommand> {
    COMMANDS.with(|commands| {
        let mut commands = commands.borrow_mut();
        (!commands.is_empty()).then(|| commands.remove(0))
    })
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
        // A left click opens the Pitot window, a right click the menu.
        TRAY_MESSAGE => {
            match lparam.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => open_window(),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_tray_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        LINK_MESSAGE => {
            match OverlayCommand::decode(wparam.0, lparam.0) {
                Some(OverlayCommand::Hello) => {
                    notify_control(ControlEvent::State {
                        hidden: HIDDEN.load(Ordering::Relaxed),
                        moving: MOVING.load(Ordering::Relaxed),
                    });
                }
                Some(OverlayCommand::SetHidden(on)) => push_command(TrayCommand::SetHidden(on)),
                Some(OverlayCommand::SetMoving(on)) => push_command(TrayCommand::SetMoving(on)),
                Some(OverlayCommand::BackgroundPreview(percent)) => {
                    push_command(TrayCommand::BackgroundOpacity {
                        percent,
                        saved: false,
                    })
                }
                Some(OverlayCommand::BackgroundSaved(percent)) => {
                    push_command(TrayCommand::BackgroundOpacity {
                        percent,
                        saved: true,
                    })
                }
                // SAFETY: posts WM_QUIT to this thread's own message queue.
                Some(OverlayCommand::Quit) => unsafe { PostQuitMessage(0) },
                None => {}
            }
            LRESULT(0)
        }
        // SAFETY: forwards unchanged arguments for default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn open_window() {
    if let Err(e) = open_control() {
        tracing::warn!("cannot open the Pitot window: {e}");
    }
}

fn show_tray_menu(hwnd: HWND) {
    // SAFETY: the menu is created, shown modally for `hwnd` (a window of this thread), and destroyed here.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let checked = |on: &AtomicBool| {
            if on.load(Ordering::Relaxed) {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            }
        };
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open Pitot"));
        // Shown in bold, as what a left click does.
        let _ = SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, checked(&MOVING), CMD_MOVE, w!("Move widgets"));
        let _ = AppendMenuW(menu, checked(&HIDDEN), CMD_HIDE, w!("Hide widgets"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_RELOAD, w!("Reload configuration"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit Pitot"));
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON;
        // With TPM_RETURNCMD the result is the chosen item's id, or 0.
        let chosen = TrackPopupMenu(menu, flags, cursor.x, cursor.y, None, hwnd, None).0;
        let _ = DestroyMenu(menu);
        match usize::try_from(chosen) {
            Ok(CMD_QUIT) => PostQuitMessage(0),
            Ok(CMD_OPEN) => open_window(),
            Ok(CMD_RELOAD) => push_command(TrayCommand::Reload),
            Ok(CMD_HIDE) => push_command(TrayCommand::SetHidden(!HIDDEN.load(Ordering::Relaxed))),
            Ok(CMD_MOVE) => push_command(TrayCommand::SetMoving(!MOVING.load(Ordering::Relaxed))),
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

/// The class of the overlay windows, registered on first use.
fn class() -> Result<PCWSTR, PlatformError> {
    static REGISTERED: OnceLock<Result<(), PlatformError>> = OnceLock::new();
    let name = w!("PitotOverlay");
    REGISTERED
        .get_or_init(|| register_class(name, Some(window_proc)))
        .clone()?;
    Ok(name)
}

/// The class of the tray window: the overlay windows' behavior under a name the Pitot window looks for.
fn tray_class() -> Result<PCWSTR, PlatformError> {
    static REGISTERED: OnceLock<Result<(), PlatformError>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| register_class(TRAY_CLASS, Some(window_proc)))
        .clone()?;
    Ok(TRAY_CLASS)
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
        Self::create_in(class()?, rect)
    }

    fn create_in(class: PCWSTR, rect: ScreenRect) -> Result<Self, PlatformError> {
        let style = OVERLAY_STYLE;
        // SAFETY: the class is registered and all strings are static; no creation parameter is passed.
        let hwnd = unsafe {
            CreateWindowExW(
                style,
                class,
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

/// A widget dragged to a new place, in screen pixels of its handle's top-left corner. `done` is set when the
/// mouse button is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetMove {
    pub id: usize,
    pub x: i32,
    pub y: i32,
    pub done: bool,
}

/// The drag in progress: the handle, where in it the pointer grabbed it, and the monitor it must stay on.
#[derive(Debug, Clone, Copy)]
struct Drag {
    hwnd: HWND,
    id: usize,
    grab: POINT,
    bounds: RECT,
}

thread_local! {
    // Handles are windows of the render thread, so their messages and these values stay on it.
    static DRAG: Cell<Option<Drag>> = const { Cell::new(None) };
    static MOVES: RefCell<Vec<WidgetMove>> = const { RefCell::new(Vec::new()) };
}

/// Appends the widget moves since the last call to `out`.
pub fn take_moves(out: &mut Vec<WidgetMove>) {
    MOVES.with(|moves| out.append(&mut moves.borrow_mut()));
}

/// Opacity of a move handle over its widget, out of 255: a tint that leaves the widget readable.
const HANDLE_ALPHA: u8 = 72;
/// Tint of a move handle, `0x00BBGGRR`.
const HANDLE_COLOR: u32 = 0x00F6_823B;

/// Client coordinates of a mouse message.
fn client_point(lparam: LPARAM) -> POINT {
    POINT {
        x: i32::from(lparam.0 as u16 as i16),
        y: i32::from((lparam.0 >> 16) as u16 as i16),
    }
}

/// Where a handle of `size` goes when the pointer is at `pointer` (screen) and grabbed it at `grab` (handle),
/// kept inside `bounds`.
fn dragged_to(pointer: POINT, grab: POINT, size: (i32, i32), bounds: RECT) -> POINT {
    // A handle larger than its monitor keeps its top-left corner on the monitor.
    POINT {
        x: (pointer.x - grab.x).clamp(bounds.left, (bounds.right - size.0).max(bounds.left)),
        y: (pointer.y - grab.y).clamp(bounds.top, (bounds.bottom - size.1).max(bounds.top)),
    }
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is a live window of this thread and `rect` is writable.
    let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
    rect
}

unsafe extern "system" fn handle_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Clicking a handle must not take the focus from the sim.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: `hwnd` is live; `info` has cbSize set as GetMonitorInfoW requires.
            let bounds = unsafe {
                let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let _ = GetMonitorInfoW(monitor, &mut info);
                // The widget's layers belong to its monitor's window, so it stays on that monitor.
                info.rcMonitor
            };
            let id = {
                // SAFETY: reads the id this handle was created with.
                let value = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
                usize::try_from(value).unwrap_or(0)
            };
            DRAG.with(|drag| {
                drag.set(Some(Drag {
                    hwnd,
                    id,
                    grab: client_point(lparam),
                    bounds,
                }));
            });
            // SAFETY: captures the mouse for this thread's window while the button is held.
            unsafe { SetCapture(hwnd) };
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(drag) = DRAG.with(Cell::get).filter(|d| d.hwnd == hwnd) {
                let rect = window_rect(hwnd);
                let point = client_point(lparam);
                let pointer = POINT {
                    x: rect.left + point.x,
                    y: rect.top + point.y,
                };
                let size = (rect.right - rect.left, rect.bottom - rect.top);
                let to = dragged_to(pointer, drag.grab, size, drag.bounds);
                if (to.x, to.y) != (rect.left, rect.top) {
                    // SAFETY: moves this thread's window without changing its size, order, or activation.
                    let _ = unsafe {
                        SetWindowPos(
                            hwnd,
                            None,
                            to.x,
                            to.y,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        )
                    };
                    push_move(drag.id, to, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            if let Some(drag) = DRAG.with(|d| d.take()).filter(|d| d.hwnd == hwnd) {
                let rect = window_rect(hwnd);
                push_move(
                    drag.id,
                    POINT {
                        x: rect.left,
                        y: rect.top,
                    },
                    true,
                );
                if msg == WM_LBUTTONUP {
                    // SAFETY: releases the capture taken on button down.
                    let _ = unsafe { ReleaseCapture() };
                }
            }
            LRESULT(0)
        }
        // A right click on a widget opens the tray menu, so moving can end there.
        WM_RBUTTONUP => {
            show_tray_menu(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: posts WM_QUIT to this thread's own message queue.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: forwards unchanged arguments for default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn push_move(id: usize, to: POINT, done: bool) {
    MOVES.with(|moves| {
        moves.borrow_mut().push(WidgetMove {
            id,
            x: to.x,
            y: to.y,
            done,
        });
    });
}

fn handle_class() -> Result<PCWSTR, PlatformError> {
    static REGISTERED: OnceLock<Result<(), PlatformError>> = OnceLock::new();
    let name = w!("PitotMoveHandle");
    REGISTERED
        .get_or_init(|| {
            // SAFETY: a stock cursor and a solid brush; the class keeps the brush for the life of the process.
            let (cursor, brush) = unsafe {
                (
                    LoadCursorW(None, IDC_SIZEALL).map_err(|e| error("LoadCursorW", &e))?,
                    CreateSolidBrush(COLORREF(HANDLE_COLOR)),
                )
            };
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(handle_proc),
                hInstance: instance()?,
                hCursor: cursor,
                hbrBackground: brush,
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

/// While widgets are being moved: a tinted window over one widget that takes the mouse there, so the widget
/// can be dragged. Clicks elsewhere still reach the windows below. Never takes the focus.
#[derive(Debug)]
pub struct MoveHandle {
    hwnd: HWND,
}

impl MoveHandle {
    /// Creates a handle over `rect`, reporting moves with `id`, above the overlay windows.
    pub fn create(id: usize, rect: ScreenRect) -> Result<Self, PlatformError> {
        let style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
        // SAFETY: the class is registered and all strings are static; no creation parameter is passed.
        let hwnd = unsafe {
            CreateWindowExW(
                style,
                handle_class()?,
                w!("Pitot widget"),
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
        let handle = Self { hwnd };
        // SAFETY: `hwnd` was just created by this thread with WS_EX_LAYERED.
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), HANDLE_ALPHA, LWA_ALPHA) }
            .map_err(|e| error("SetLayeredWindowAttributes", &e))?;
        // SAFETY: stores the id in the window's user data slot, read back on button down.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, id as isize) };
        // SAFETY: shows this thread's window without activating it, at the top of the topmost windows.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        Ok(handle)
    }
}

impl Drop for MoveHandle {
    fn drop(&mut self) {
        DRAG.with(|drag| {
            if drag.get().is_some_and(|d| d.hwnd == self.hwnd) {
                drag.set(None);
            }
        });
        // SAFETY: the window belongs to this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// The notification-area icon, owned by a hidden window that the Pitot window finds by its class. A left click
/// opens the Pitot window; a right click opens the menu (Open Pitot, Move widgets, Hide widgets, Reload
/// configuration, Quit).
#[derive(Debug)]
pub struct Tray {
    window: OverlayWindow,
}

impl Tray {
    pub fn create() -> Result<Self, PlatformError> {
        let window = OverlayWindow::create_in(tray_class()?, ScreenRect::default())?;
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

    /// Shows *Move widgets* checked or not the next time the menu opens.
    pub fn set_moving(&self, moving: bool) {
        MOVING.store(moving, Ordering::Relaxed);
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
/// Name of the mutex that marks an open Pitot window in this Windows session.
const CONTROL_MUTEX: PCWSTR = w!("Local\\PitotControl");

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

    /// Takes the Pitot window's instance lock. `Ok(None)` means a Pitot window is already open in this session.
    pub fn acquire_control() -> Result<Option<Self>, PlatformError> {
        Self::acquire_named(CONTROL_MUTEX)
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
    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    use super::*;

    #[test]
    fn dragged_handles_stay_on_their_monitor() {
        let bounds = RECT {
            left: -1080,
            top: 0,
            right: 0,
            bottom: 1920,
        };
        let at = |x, y| POINT { x, y };
        let to = |pointer| {
            let p = dragged_to(pointer, at(10, 5), (300, 100), bounds);
            (p.x, p.y)
        };
        assert_eq!(to(at(-500, 400)), (-510, 395));
        assert_eq!(to(at(-2000, -50)), (-1080, 0), "clamped to the top-left");
        assert_eq!(
            to(at(50, 5000)),
            (-300, 1820),
            "clamped to the bottom-right"
        );
        let small = RECT {
            left: 0,
            top: 0,
            right: 200,
            bottom: 50,
        };
        let p = dragged_to(at(150, 40), at(0, 0), (300, 100), small);
        assert_eq!((p.x, p.y), (0, 0), "larger than the monitor");
    }

    #[test]
    fn a_handle_reports_its_drag() {
        let handle = MoveHandle::create(
            7,
            ScreenRect {
                x: 100,
                y: 100,
                width: 40,
                height: 30,
            },
        )
        .expect("handle");
        let lparam = |x: i32, y: i32| LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
        let send = |msg, x, y| {
            // SAFETY: sends a mouse message to a window of this thread; its procedure runs synchronously.
            unsafe { SendMessageW(handle.hwnd, msg, None, Some(lparam(x, y))) };
        };
        send(WM_LBUTTONDOWN, 10, 10);
        send(WM_MOUSEMOVE, 60, 30);
        send(WM_LBUTTONUP, 0, 0);
        let mut moves = Vec::new();
        take_moves(&mut moves);
        let moved = |x, y, done| WidgetMove { id: 7, x, y, done };
        assert_eq!(moves, [moved(150, 120, false), moved(150, 120, true)]);
        let rect = window_rect(handle.hwnd);
        assert_eq!((rect.left, rect.top), (150, 120), "the handle follows");
        take_moves(&mut moves);
        assert_eq!(moves.len(), 2, "taken once");
    }

    #[test]
    fn tray_choices_reach_the_app_once_in_order() {
        push_command(TrayCommand::SetHidden(true));
        push_command(TrayCommand::Reload);
        assert_eq!(take_tray_command(), Some(TrayCommand::SetHidden(true)));
        assert_eq!(take_tray_command(), Some(TrayCommand::Reload));
        assert_eq!(take_tray_command(), None, "taken once");
    }

    #[test]
    fn the_pitot_window_reaches_the_app_through_the_tray_window() {
        // A window of the tray's class, without the icon. Messages are sent to it directly rather than found by
        // class, so an overlay running on this machine is not involved.
        let window = OverlayWindow::create_in(tray_class().expect("class"), ScreenRect::default())
            .expect("window");
        let send = |command: OverlayCommand| {
            let (c, v) = command.encode();
            // SAFETY: sends to a window of this thread; its procedure runs synchronously.
            unsafe { SendMessageW(window.hwnd, LINK_MESSAGE, Some(WPARAM(c)), Some(LPARAM(v))) };
        };
        send(OverlayCommand::SetMoving(true));
        send(OverlayCommand::SetHidden(false));
        send(OverlayCommand::BackgroundPreview(30));
        send(OverlayCommand::BackgroundSaved(31));
        assert_eq!(take_tray_command(), Some(TrayCommand::SetMoving(true)));
        assert_eq!(take_tray_command(), Some(TrayCommand::SetHidden(false)));
        let background = |percent, saved| Some(TrayCommand::BackgroundOpacity { percent, saved });
        assert_eq!(take_tray_command(), background(30, false));
        assert_eq!(take_tray_command(), background(31, true));
        assert_eq!(take_tray_command(), None);
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
