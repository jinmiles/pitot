//! Win32 platform layer: DPI awareness, monitors, the widget windows' styles and placement, the tray icon and its
//! timer, the move handles, the single-instance lock, the configuration watch, and the [`link`] to the Pitot
//! window.
//!
//! GPUI owns the message loop: its `GetMessageW` loop dispatches the messages of every window of its thread, so
//! the tray window and the move handles, created on that thread, get theirs there. Their window procedures
//! queue what happened and wake the scheduler ([`set_wake`]); they never call into GPUI. All Win32 `unsafe` for
//! windowing lives here; callers use the safe types below.

mod link;
mod monitors;

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

pub use link::{
    ControlEvent, ControlLink, OverlayCommand, WINDOW_FLAG, activate_control, notify_control,
    open_control, overlay_running, quit_overlay_and_wait, send_to_overlay, start_overlay,
};
use link::{LINK_MESSAGE, TRAY_CLASS, register_class};
pub use monitors::{Monitor, displays, monitors};

use windows::Win32::Foundation::{
    COLORREF, CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, FrameRect, GetMonitorInfoW, GetStockObject,
    HBRUSH, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, WHITE_BRUSH,
};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForMultipleObjects,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::{
    FOLDERID_Documents, KF_FLAG_DEFAULT, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW, SHGetKnownFolderPath, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_HREDRAW, CS_VREDRAW, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow, GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetCursorPos,
    GetWindowLongPtrW, GetWindowRect, HTCLIENT, HWND_TOPMOST, IDC_SIZEALL, IDC_SIZENESW,
    IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDI_APPLICATION, KillTimer, LWA_ALPHA, LoadCursorW,
    LoadIconW, MA_NOACTIVATE, MF_CHECKED, MF_SEPARATOR, MF_STRING, PostQuitMessage,
    RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetCursor, SetForegroundWindow,
    SetLayeredWindowAttributes, SetMenuDefaultItem, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WINDOW_EX_STYLE,
    WM_APP, WM_CAPTURECHANGED, WM_CLOSE, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER,
    WNDCLASSEXW, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_APPWINDOW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::wake::{Wake, reason};

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

/// Opts the process into per-monitor DPI awareness, so window and monitor coordinates are physical pixels.
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
    /// The Pitot window's text size slider, in percent: moved, or saved into `config.toml`.
    TextSize {
        percent: u8,
        saved: bool,
    },
}

thread_local! {
    // The tray window belongs to the app thread, so its commands queue there.
    static COMMANDS: RefCell<Vec<TrayCommand>> = const { RefCell::new(Vec::new()) };
}

/// The scheduler's wake-up, which the window procedures here signal: set once at start with [`set_wake`].
static WAKE: OnceLock<Arc<Wake>> = OnceLock::new();

/// Sets the wake-up that tray commands, move-handle drags, and the tray timer signal.
pub fn set_wake(wake: Arc<Wake>) {
    let _ = WAKE.set(wake);
}

fn wake(reasons: u32) {
    if let Some(wake) = WAKE.get() {
        wake.signal(reasons);
    }
}

fn push_command(command: TrayCommand) {
    COMMANDS.with(|commands| commands.borrow_mut().push(command));
    wake(reason::COMMAND);
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
        // The scheduler's timer (see `Tray::set_timer`), once.
        WM_TIMER if wparam.0 == TIMER_ID => {
            // SAFETY: stops a timer of this thread's own window.
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_ID) };
            wake(reason::TIMER);
            LRESULT(0)
        }
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
                Some(OverlayCommand::TextSizePreview(percent)) => {
                    push_command(TrayCommand::TextSize {
                        percent,
                        saved: false,
                    })
                }
                Some(OverlayCommand::TextSizeSaved(percent)) => {
                    push_command(TrayCommand::TextSize {
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

/// The class of the tray window, registered on first use, under a name the Pitot window looks for.
fn tray_class() -> Result<PCWSTR, PlatformError> {
    static REGISTERED: OnceLock<Result<(), PlatformError>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| register_class(TRAY_CLASS, Some(window_proc)))
        .clone()?;
    Ok(TRAY_CLASS)
}

/// A top-level window that is never shown, for messages: the tray icon's, the Pitot window's, and the timer's.
#[derive(Debug)]
struct HiddenWindow {
    hwnd: HWND,
}

impl HiddenWindow {
    fn create(class: PCWSTR) -> Result<Self, PlatformError> {
        // SAFETY: the class is registered and all strings are static; no creation parameter is passed. Created
        // without WS_VISIBLE and never shown.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class,
                w!("Pitot"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance()?),
                None,
            )
        }
        .map_err(|e| error("CreateWindowExW", &e))?;
        Ok(Self { hwnd })
    }
}

impl Drop for HiddenWindow {
    fn drop(&mut self) {
        // SAFETY: the window belongs to this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

fn hwnd_of(raw: isize) -> HWND {
    HWND(raw as *mut _)
}

/// Makes a widget's GPUI window an overlay window: a plain popup (without the caption GPUI's frameless windows
/// keep, which gives them the desktop's shadow and rounded corners), click-through, always on top, never
/// activated, and without a taskbar button. Only `WS_EX_LAYERED` together with `WS_EX_TRANSPARENT` takes a window
/// out of mouse hit testing, so clicks reach the windows below, also those of other processes. The layer is set to
/// full opacity once, which a layered window needs before it shows; GPUI's composition swap chain still draws the
/// content with per-pixel alpha. Call it while GPUI's app is not borrowed: the window procedure runs GPUI's
/// handlers.
pub fn style_widget_window(raw: isize) -> Result<(), PlatformError> {
    let hwnd = hwnd_of(raw);
    // SAFETY: `hwnd` is a live window of this thread, made by GPUI and kept until it closes; style changes and a
    // frame change for it.
    unsafe {
        let ex = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
        let ex = WINDOW_EX_STYLE(ex.0 & !WS_EX_APPWINDOW.0)
            | WS_EX_LAYERED
            | WS_EX_TRANSPARENT
            | WS_EX_TOPMOST
            | WS_EX_NOACTIVATE
            | WS_EX_TOOLWINDOW;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex.0 as isize);
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (WS_POPUP | WS_CLIPSIBLINGS | WS_CLIPCHILDREN).0 as isize,
        );
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)
            .map_err(|e| error("SetLayeredWindowAttributes", &e))?;
        let corners = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const corners).cast(),
            size_of_val(&corners) as u32,
        );
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )
        .map_err(|e| error("SetWindowPos", &e))
    }
}

/// Puts a widget window at `rect` (physical pixels) without changing its order or activating it, and shows it
/// when `show`. Moving it to a monitor of another DPI makes GPUI size it for that DPI, so it is placed once more.
/// Call it while GPUI's app is not borrowed.
pub fn place_widget_window(raw: isize, rect: ScreenRect, show: bool) {
    let hwnd = hwnd_of(raw);
    let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
    if show {
        flags |= SWP_SHOWWINDOW;
    }
    for _ in 0..2 {
        // SAFETY: moves and sizes this thread's window without changing its order or activation.
        let _ = unsafe { SetWindowPos(hwnd, None, rect.x, rect.y, rect.width, rect.height, flags) };
        let placed = window_rect(hwnd);
        let (width, height) = size_of_rect(placed);
        if (placed.left, placed.top, width, height) == (rect.x, rect.y, rect.width, rect.height) {
            break;
        }
    }
}

/// Shows a widget window without activating it, or hides it. Call it while GPUI's app is not borrowed.
pub fn show_widget_window(raw: isize, show: bool) {
    let command = if show { SW_SHOWNOACTIVATE } else { SW_HIDE };
    // SAFETY: shows or hides this thread's window; it never takes the focus.
    let _ = unsafe { ShowWindow(hwnd_of(raw), command) };
}

/// Ends GPUI's message loop, as the tray's *Quit Pitot* does.
pub fn quit() {
    // SAFETY: posts WM_QUIT to this thread's own message queue.
    unsafe { PostQuitMessage(0) };
}

/// A widget dragged to a new place or size: its handle's top-left corner and size, in screen pixels. `done` is set
/// when the mouse button is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidgetMove {
    pub id: usize,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub done: bool,
}

/// The edges of a handle a drag moves: all four to move the widget, one or two to resize it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Edges {
    left: bool,
    top: bool,
    right: bool,
    bottom: bool,
}

impl Edges {
    const MOVE: Self = Self {
        left: true,
        top: true,
        right: true,
        bottom: true,
    };

    /// The cursor that shows what a press here does.
    fn cursor(self) -> PCWSTR {
        match (self.left, self.top, self.right, self.bottom) {
            (true, true, true, true) => IDC_SIZEALL,
            (true, true, ..) | (_, _, true, true) => IDC_SIZENWSE,
            (true, _, _, true) | (_, true, true, _) => IDC_SIZENESW,
            (true, ..) | (_, _, true, _) => IDC_SIZEWE,
            _ => IDC_SIZENS,
        }
    }
}

/// Pixels along a handle's edges that resize it, and the side of its corners, the grip at the bottom right among
/// them.
const EDGE: i32 = 6;
const GRIP: i32 = 16;

/// What a press at `point` (in the handle) of a handle of `size` drags: an edge, a corner, or the whole widget.
fn edges_at(point: POINT, size: (i32, i32)) -> Edges {
    let (w, h) = size;
    // A small handle keeps most of its area for moving.
    let edge = EDGE.min(w / 4).min(h / 4);
    let corner = GRIP.min(w / 3).min(h / 3);
    let mut edges = Edges {
        left: point.x < edge,
        top: point.y < edge,
        right: point.x >= w - edge,
        bottom: point.y >= h - edge,
    };
    // Near a corner, an edge takes the corner's other edge too.
    if edges.left || edges.right {
        edges.top |= point.y < corner;
        edges.bottom |= point.y >= h - corner;
    }
    if edges.top || edges.bottom {
        edges.left |= point.x < corner;
        edges.right |= point.x >= w - corner;
    }
    // The grip.
    if point.x >= w - corner && point.y >= h - corner {
        (edges.right, edges.bottom) = (true, true);
    }
    if edges == Edges::default() {
        Edges::MOVE
    } else {
        edges
    }
}

/// The drag in progress: the handle, the edges it moves, its rectangle and the pointer (screen) when the button
/// went down, the monitor it must stay on, and the widget's smallest size.
#[derive(Debug, Clone, Copy)]
struct Drag {
    hwnd: HWND,
    id: usize,
    edges: Edges,
    start: RECT,
    press: POINT,
    bounds: RECT,
    min: (i32, i32),
}

/// A handle's widget and the widget's smallest size.
#[derive(Debug, Clone, Copy)]
struct HandleInfo {
    hwnd: HWND,
    id: usize,
    min: (i32, i32),
}

thread_local! {
    // Handles are windows of the render thread, so their messages and these values stay on it.
    static DRAG: Cell<Option<Drag>> = const { Cell::new(None) };
    static MOVES: RefCell<Vec<WidgetMove>> = const { RefCell::new(Vec::new()) };
    static HANDLES: RefCell<Vec<HandleInfo>> = const { RefCell::new(Vec::new()) };
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

/// The handle's rectangle once the pointer moved by `delta` since the press. Moving keeps its size and its monitor.
/// Resizing moves only the dragged edges, each up to the monitor's edge and no closer to the opposite edge than the
/// widget's smallest size.
fn dragged(drag: &Drag, delta: POINT) -> RECT {
    let start = drag.start;
    let size = size_of_rect(start);
    if drag.edges == Edges::MOVE {
        let pointer = POINT {
            x: drag.press.x + delta.x,
            y: drag.press.y + delta.y,
        };
        let grab = POINT {
            x: drag.press.x - start.left,
            y: drag.press.y - start.top,
        };
        let to = dragged_to(pointer, grab, size, drag.bounds);
        return RECT {
            left: to.x,
            top: to.y,
            right: to.x + size.0,
            bottom: to.y + size.1,
        };
    }
    let (bounds, (min_w, min_h)) = (drag.bounds, drag.min);
    let mut rect = start;
    // The smallest size wins over the monitor's edge, so the two limits never cross.
    if drag.edges.left {
        rect.left = (start.left + delta.x)
            .max(bounds.left)
            .min(start.right - min_w);
    }
    if drag.edges.right {
        rect.right = (start.right + delta.x)
            .min(bounds.right)
            .max(start.left + min_w);
    }
    if drag.edges.top {
        rect.top = (start.top + delta.y)
            .max(bounds.top)
            .min(start.bottom - min_h);
    }
    if drag.edges.bottom {
        rect.bottom = (start.bottom + delta.y)
            .min(bounds.bottom)
            .max(start.top + min_h);
    }
    rect
}

fn window_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is a live window of this thread and `rect` is writable.
    let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
    rect
}

fn size_of_rect(rect: RECT) -> (i32, i32) {
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// Where the pointer is in `hwnd`.
fn pointer_in(hwnd: HWND) -> POINT {
    let mut cursor = POINT::default();
    // SAFETY: writes the cursor position into a local.
    let _ = unsafe { GetCursorPos(&mut cursor) };
    let rect = window_rect(hwnd);
    POINT {
        x: cursor.x - rect.left,
        y: cursor.y - rect.top,
    }
}

/// Paints a handle: the tint (the class brush, which `BeginPaint` erases with), a thin frame along the edges that
/// resize, and dots on the grip in the bottom-right corner.
fn paint_handle(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    // SAFETY: paints this thread's window between BeginPaint and EndPaint with a stock brush, which is never freed.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut paint);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let white = HBRUSH(GetStockObject(WHITE_BRUSH).0);
        FrameRect(hdc, &client, white);
        let (w, h) = size_of_rect(client);
        for row in 0..3 {
            for column in 0..3 {
                if row + column < 2 {
                    continue;
                }
                let (x, y) = (w - 15 + column * 5, h - 15 + row * 5);
                let dot = RECT {
                    left: x,
                    top: y,
                    right: x + 3,
                    bottom: y + 3,
                };
                FillRect(hdc, &dot, white);
            }
        }
        let _ = EndPaint(hwnd, &paint);
    }
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
        // The cursor says what a press does: move, or resize from an edge or a corner.
        WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
            let edges = edges_at(pointer_in(hwnd), size_of_rect(window_rect(hwnd)));
            // SAFETY: a stock cursor, which the system shares and never frees.
            unsafe {
                if let Ok(cursor) = LoadCursorW(None, edges.cursor()) {
                    SetCursor(Some(cursor));
                }
            }
            LRESULT(1)
        }
        WM_PAINT => {
            paint_handle(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let found = HANDLES.with(|h| h.borrow().iter().find(|i| i.hwnd == hwnd).copied());
            let Some(info) = found else {
                return LRESULT(0);
            };
            let mut monitor = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: `hwnd` is live; `monitor` has cbSize set as GetMonitorInfoW requires.
            let bounds = unsafe {
                let on = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let _ = GetMonitorInfoW(on, &mut monitor);
                // The widget's layers belong to its monitor's window, so it stays on that monitor.
                monitor.rcMonitor
            };
            let start = window_rect(hwnd);
            let point = client_point(lparam);
            DRAG.with(|drag| {
                drag.set(Some(Drag {
                    hwnd,
                    id: info.id,
                    edges: edges_at(point, size_of_rect(start)),
                    start,
                    press: POINT {
                        x: start.left + point.x,
                        y: start.top + point.y,
                    },
                    bounds,
                    min: info.min,
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
                let delta = POINT {
                    x: rect.left + point.x - drag.press.x,
                    y: rect.top + point.y - drag.press.y,
                };
                let to = dragged(&drag, delta);
                if to != rect {
                    let (width, height) = size_of_rect(to);
                    // SAFETY: moves and sizes this thread's window without changing its order or activation.
                    let _ = unsafe {
                        SetWindowPos(
                            hwnd,
                            None,
                            to.left,
                            to.top,
                            width,
                            height,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        )
                    };
                    push_move(drag.id, to, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            if let Some(drag) = DRAG.with(|d| d.take()).filter(|d| d.hwnd == hwnd) {
                push_move(drag.id, window_rect(hwnd), true);
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

fn push_move(id: usize, to: RECT, done: bool) {
    let (width, height) = size_of_rect(to);
    MOVES.with(|moves| {
        moves.borrow_mut().push(WidgetMove {
            id,
            x: to.left,
            y: to.top,
            width,
            height,
            done,
        });
    });
    wake(reason::MOVE);
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
                // Painted whole after a resize, so the frame and the grip follow the edges.
                style: CS_HREDRAW | CS_VREDRAW,
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
/// can be dragged to another place, or resized from its edges and corners. Clicks elsewhere still reach the
/// windows below. Never takes the focus.
#[derive(Debug)]
pub struct MoveHandle {
    hwnd: HWND,
    id: usize,
}

impl MoveHandle {
    /// Creates a handle over `rect`, reporting moves with `id`, above the overlay windows. Resizing it keeps at least
    /// `min`, the widget's smallest size.
    pub fn create(id: usize, rect: ScreenRect, min: (u32, u32)) -> Result<Self, PlatformError> {
        Self::create_shown(id, rect, min, true)
    }

    /// [`MoveHandle::create`], left hidden unless `show`: a hidden handle handles the same messages, which the
    /// tests send it, and shows nothing on any monitor.
    fn create_shown(
        id: usize,
        rect: ScreenRect,
        min: (u32, u32),
        show: bool,
    ) -> Result<Self, PlatformError> {
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
        let handle = Self { hwnd, id };
        let min = (
            i32::try_from(min.0).unwrap_or(i32::MAX),
            i32::try_from(min.1).unwrap_or(i32::MAX),
        );
        HANDLES.with(|h| h.borrow_mut().push(HandleInfo { hwnd, id, min }));
        // SAFETY: `hwnd` was just created by this thread with WS_EX_LAYERED.
        unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), HANDLE_ALPHA, LWA_ALPHA) }
            .map_err(|e| error("SetLayeredWindowAttributes", &e))?;
        if !show {
            return Ok(handle);
        }
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

    /// The id its moves are reported with.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Puts the handle over `rect`, for a widget that took another size than the one dragged to.
    pub fn place(&self, rect: ScreenRect) {
        // SAFETY: moves and sizes this thread's window without changing its order or activation.
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
}

impl Drop for MoveHandle {
    fn drop(&mut self) {
        DRAG.with(|drag| {
            if drag.get().is_some_and(|d| d.hwnd == self.hwnd) {
                drag.set(None);
            }
        });
        HANDLES.with(|h| h.borrow_mut().retain(|i| i.hwnd != self.hwnd));
        // SAFETY: the window belongs to this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// The notification-area icon, owned by a hidden window that the Pitot window finds by its class. A left click
/// opens the Pitot window; a right click opens the menu (Open Pitot, Move widgets, Hide widgets, Reload
/// configuration, Quit). The same window carries the scheduler's timer.
#[derive(Debug)]
pub struct Tray {
    window: HiddenWindow,
}

/// The id of the scheduler's timer on the tray window.
const TIMER_ID: usize = 1;

impl Tray {
    pub fn create() -> Result<Self, PlatformError> {
        let window = HiddenWindow::create(tray_class()?)?;
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

    /// Wakes the scheduler once after `delay` (with the timer resolution of the system, about 16 ms), replacing the
    /// time set before. A delay of zero wakes it at once. A window timer: it fires on GPUI's thread without a
    /// thread of its own, and `timeBeginPeriod` is never called.
    pub fn set_timer(&self, delay: Duration) {
        if delay.is_zero() {
            wake(reason::TIMER);
            return;
        }
        let millis = u32::try_from(delay.as_millis()).unwrap_or(u32::MAX).max(1);
        // SAFETY: sets a timer of this thread's own window; setting it again with the same id replaces it.
        unsafe { SetTimer(Some(self.window.hwnd), TIMER_ID, millis, None) };
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
        // SAFETY: removes the icon added in `create` for the same window and id, and the timer of the window.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            let _ = KillTimer(Some(self.window.hwnd), TIMER_ID);
        }
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
/// polling. A thread of its own waits on it and calls `changed` for each change; GPUI's message loop cannot wait
/// on it.
#[derive(Debug)]
pub struct DirectoryWatch {
    /// Ends the thread.
    stop: HANDLE,
    thread: Option<JoinHandle<()>>,
}

impl DirectoryWatch {
    pub fn start(
        dir: &std::path::Path,
        changed: impl Fn() + Send + 'static,
    ) -> Result<Self, PlatformError> {
        let path = windows::core::HSTRING::from(dir.as_os_str());
        // SAFETY: `path` is a NUL-terminated wide string that outlives the call.
        let change = unsafe {
            FindFirstChangeNotificationW(
                &path,
                false,
                FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
            )
        }
        .map_err(|e| error("FindFirstChangeNotificationW", &e))?;
        // SAFETY: an unnamed manual-reset event with default security.
        let stop = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
            Ok(stop) => stop,
            Err(e) => {
                // SAFETY: closes the notification opened above, once.
                let _ = unsafe { FindCloseChangeNotification(change) };
                return Err(error("CreateEventW", &e));
            }
        };
        // Handles are plain kernel object values; the thread owns the notification and borrows the stop event,
        // which `Drop` closes only after the thread has ended.
        let (change_raw, stop_raw) = (change.0 as isize, stop.0 as isize);
        let thread = std::thread::Builder::new()
            .name("pitot-watch".into())
            .spawn(move || {
                let handles = [HANDLE(change_raw as *mut _), HANDLE(stop_raw as *mut _)];
                loop {
                    // SAFETY: waits on two live handles: the notification this thread owns and the stop event.
                    let result = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                    if result != WAIT_OBJECT_0 {
                        break;
                    }
                    changed();
                    // SAFETY: re-arms the notification this thread owns.
                    if unsafe { FindNextChangeNotification(handles[0]) }.is_err() {
                        break;
                    }
                }
                // SAFETY: closes the notification this thread owns, once.
                let _ = unsafe { FindCloseChangeNotification(handles[0]) };
            })
            .map_err(|_| PlatformError {
                call: "thread spawn",
                code: 0,
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for DirectoryWatch {
    fn drop(&mut self) {
        // SAFETY: signals the stop event owned by `self`.
        let _ = unsafe { SetEvent(self.stop) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: closes the stop event owned by `self`, once, after the thread that waited on it ended.
        let _ = unsafe { CloseHandle(self.stop) };
    }
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
    fn edges_and_corners_resize_and_the_rest_moves() {
        let size = (300, 100);
        let at = |x, y| edges_at(POINT { x, y }, size);
        let edges = |left, top, right, bottom| Edges {
            left,
            top,
            right,
            bottom,
        };
        assert_eq!(at(150, 50), Edges::MOVE);
        assert_eq!(at(6, 6), Edges::MOVE, "just inside the edges");
        assert_eq!(at(2, 50), edges(true, false, false, false));
        assert_eq!(at(150, 97), edges(false, false, false, true));
        assert_eq!(at(299, 50), edges(false, false, true, false));
        assert_eq!(at(150, 0), edges(false, true, false, false));
        // Corners reach along the edges, and the grip is a square at the bottom right.
        assert_eq!(at(2, 10), edges(true, true, false, false));
        assert_eq!(at(290, 2), edges(false, true, true, false));
        assert_eq!(at(287, 88), edges(false, false, true, true), "the grip");
        assert_eq!(at(2, 95), edges(true, false, false, true));
        assert_eq!(Edges::MOVE.cursor(), IDC_SIZEALL);
        assert_eq!(at(287, 88).cursor(), IDC_SIZENWSE);
        assert_eq!(at(290, 2).cursor(), IDC_SIZENESW);
        assert_eq!(at(2, 50).cursor(), IDC_SIZEWE);
        assert_eq!(at(150, 97).cursor(), IDC_SIZENS);
        // A tiny handle still moves from its middle.
        assert_eq!(edges_at(POINT { x: 10, y: 6 }, (20, 12)), Edges::MOVE);
    }

    #[test]
    fn resizing_keeps_the_monitor_and_the_smallest_size() {
        let drag = |edges| Drag {
            hwnd: HWND::default(),
            id: 0,
            edges,
            start: RECT {
                left: 100,
                top: 100,
                right: 400,
                bottom: 300,
            },
            press: POINT { x: 0, y: 0 },
            bounds: RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            min: (200, 120),
        };
        let rect = |edges, dx, dy| {
            let r = dragged(&drag(edges), POINT { x: dx, y: dy });
            (r.left, r.top, r.right, r.bottom)
        };
        let corner = Edges {
            right: true,
            bottom: true,
            ..Edges::default()
        };
        assert_eq!(rect(corner, 50, 40), (100, 100, 450, 340));
        assert_eq!(
            rect(corner, 5000, 5000),
            (100, 100, 1920, 1080),
            "the monitor's edge"
        );
        assert_eq!(
            rect(corner, -500, -500),
            (100, 100, 300, 220),
            "the smallest size"
        );
        let top_left = Edges {
            left: true,
            top: true,
            ..Edges::default()
        };
        assert_eq!(rect(top_left, -30, -20), (70, 80, 400, 300));
        assert_eq!(rect(top_left, -500, -500), (0, 0, 400, 300));
        assert_eq!(rect(top_left, 500, 500), (200, 180, 400, 300));
        let right = Edges {
            right: true,
            ..Edges::default()
        };
        assert_eq!(rect(right, 20, 99), (100, 100, 420, 300), "one edge only");
        // A move keeps the size.
        assert_eq!(rect(Edges::MOVE, 20, -500), (120, 0, 420, 200));
    }

    #[test]
    fn a_handle_reports_its_drag() {
        // Hidden, so nothing shows on the monitor it is placed on.
        let handle = MoveHandle::create_shown(
            7,
            ScreenRect {
                x: 100,
                y: 100,
                width: 40,
                height: 30,
            },
            (20, 20),
            false,
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
        let moved = |x, y, done| WidgetMove {
            id: 7,
            x,
            y,
            width: 40,
            height: 30,
            done,
        };
        assert_eq!(moves, [moved(150, 120, false), moved(150, 120, true)]);
        let rect = window_rect(handle.hwnd);
        assert_eq!((rect.left, rect.top), (150, 120), "the handle follows");
        take_moves(&mut moves);
        assert_eq!(moves.len(), 2, "taken once");
    }

    #[test]
    fn a_handle_resizes_from_its_grip() {
        // Hidden and far off every screen; the nearest monitor's edges are far away on the right and below.
        let handle = MoveHandle::create_shown(
            3,
            ScreenRect {
                x: -30_000,
                y: -30_000,
                width: 100,
                height: 80,
            },
            (60, 40),
            false,
        )
        .expect("handle");
        assert_eq!(handle.id(), 3);
        let lparam = |x: i32, y: i32| LPARAM(((y as isize) << 16) | (x as isize & 0xFFFF));
        let send = |msg, x, y| {
            // SAFETY: sends a mouse message to a window of this thread; its procedure runs synchronously.
            unsafe { SendMessageW(handle.hwnd, msg, None, Some(lparam(x, y))) };
        };
        // The grip, then 30 px right and 20 px down; then past the smallest size.
        send(WM_LBUTTONDOWN, 95, 75);
        send(WM_MOUSEMOVE, 125, 95);
        let rect = window_rect(handle.hwnd);
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (-30_000, -30_000, -29_870, -29_900),
            "the corner follows"
        );
        send(WM_MOUSEMOVE, 0, 0);
        send(WM_LBUTTONUP, 0, 0);
        let mut moves = Vec::new();
        take_moves(&mut moves);
        let sized = |width, height, done| WidgetMove {
            id: 3,
            x: -30_000,
            y: -30_000,
            width,
            height,
            done,
        };
        assert_eq!(
            moves,
            [
                sized(130, 100, false),
                sized(60, 40, false),
                sized(60, 40, true)
            ]
        );
        handle.place(ScreenRect {
            x: -30_000,
            y: -30_000,
            width: 70,
            height: 50,
        });
        assert_eq!(size_of_rect(window_rect(handle.hwnd)), (70, 50));
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
        let window = HiddenWindow::create(tray_class().expect("class")).expect("window");
        let send = |command: OverlayCommand| {
            let (c, v) = command.encode();
            // SAFETY: sends to a window of this thread; its procedure runs synchronously.
            unsafe { SendMessageW(window.hwnd, LINK_MESSAGE, Some(WPARAM(c)), Some(LPARAM(v))) };
        };
        send(OverlayCommand::SetMoving(true));
        send(OverlayCommand::SetHidden(false));
        send(OverlayCommand::BackgroundPreview(30));
        send(OverlayCommand::BackgroundSaved(31));
        send(OverlayCommand::TextSizePreview(125));
        send(OverlayCommand::TextSizeSaved(130));
        assert_eq!(take_tray_command(), Some(TrayCommand::SetMoving(true)));
        assert_eq!(take_tray_command(), Some(TrayCommand::SetHidden(false)));
        let background = |percent, saved| Some(TrayCommand::BackgroundOpacity { percent, saved });
        assert_eq!(take_tray_command(), background(30, false));
        assert_eq!(take_tray_command(), background(31, true));
        let text = |percent, saved| Some(TrayCommand::TextSize { percent, saved });
        assert_eq!(take_tray_command(), text(125, false));
        assert_eq!(take_tray_command(), text(130, true));
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
