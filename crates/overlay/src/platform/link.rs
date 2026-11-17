//! The link between the overlay and the Pitot window, which runs as its own process: the same executable started
//! with [`WINDOW_FLAG`].
//!
//! Each side owns a window the other finds by class name: the overlay's tray window (`PitotTray`, a hidden
//! top-level window) and the Pitot window's message-only window (`PitotControlLink`). A message carries a
//! command in `wParam` and its value in `lParam`, so nothing is allocated and nothing else is shared. The overlay
//! handles these messages on its message-loop thread, between frames, only when one arrives.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WAIT_OBJECT_0,
    WPARAM,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FindWindowExW, FindWindowW, GWL_EXSTYLE, GetForegroundWindow, GetMessageW, GetWindowLongPtrW,
    GetWindowThreadProcessId, HWND_MESSAGE, HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, SW_RESTORE, SW_SHOW, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WNDCLASSEXW, WNDPROC,
    WS_EX_TOPMOST,
};
use windows::core::{PCWSTR, w};

use super::{PlatformError, error, instance, last_error};
use crate::config::{MAX_BACKGROUND_OPACITY, MAX_TEXT_SIZE, MIN_TEXT_SIZE};

/// The message both sides use; window classes keep it from reaching anything else.
pub(super) const LINK_MESSAGE: u32 = WM_APP + 2;
/// Class of the overlay's tray window.
pub(super) const TRAY_CLASS: PCWSTR = w!("PitotTray");
/// Class of the Pitot window's message-only window.
const CONTROL_CLASS: PCWSTR = w!("PitotControlLink");

/// The argument that makes Pitot's executable run the Pitot window. Without it, the executable runs the overlay.
pub const WINDOW_FLAG: &str = "--window";

/// A request to the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayCommand {
    /// Answer with [`ControlEvent::State`].
    Hello,
    /// Hide or show the widgets, as *Hide widgets* in the tray menu does.
    SetHidden(bool),
    /// Start or end moving widgets, as *Move widgets* does.
    SetMoving(bool),
    /// Quit the overlay, which closes the Pitot window too.
    Quit,
    /// Draw the widgets' backgrounds at this `[general] background_opacity` (a percent, at most 100), while the
    /// window's slider moves. It holds over the file's value until [`OverlayCommand::BackgroundSaved`].
    BackgroundPreview(u8),
    /// The window wrote `[general] background_opacity` into `config.toml`, which now holds this percent.
    BackgroundSaved(u8),
    /// Lay the widgets out at this `[general] text_size` (a percent from 70 to 150), while the window's slider
    /// moves. It holds over the file's value until [`OverlayCommand::TextSizeSaved`].
    TextSizePreview(u8),
    /// The window wrote `[general] text_size` into `config.toml`, which now holds this percent.
    TextSizeSaved(u8),
}

impl OverlayCommand {
    pub(super) fn encode(self) -> (usize, isize) {
        match self {
            Self::Hello => (1, 0),
            Self::SetHidden(on) => (2, isize::from(on)),
            Self::SetMoving(on) => (3, isize::from(on)),
            Self::Quit => (4, 0),
            Self::BackgroundPreview(percent) => (5, isize::from(percent)),
            Self::BackgroundSaved(percent) => (6, isize::from(percent)),
            Self::TextSizePreview(percent) => (7, isize::from(percent)),
            Self::TextSizeSaved(percent) => (8, isize::from(percent)),
        }
    }

    pub(super) fn decode(command: usize, value: isize) -> Option<Self> {
        // Any value is read as a percent in the setting's range.
        let percent = || value.clamp(0, isize::from(MAX_BACKGROUND_OPACITY)) as u8;
        let text_size =
            || value.clamp(isize::from(MIN_TEXT_SIZE), isize::from(MAX_TEXT_SIZE)) as u8;
        match command {
            1 => Some(Self::Hello),
            2 => Some(Self::SetHidden(value != 0)),
            3 => Some(Self::SetMoving(value != 0)),
            4 => Some(Self::Quit),
            5 => Some(Self::BackgroundPreview(percent())),
            6 => Some(Self::BackgroundSaved(percent())),
            7 => Some(Self::TextSizePreview(text_size())),
            8 => Some(Self::TextSizeSaved(text_size())),
            _ => None,
        }
    }
}

/// News for the Pitot window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEvent {
    /// The overlay runs, with its widgets hidden or not and being moved or not.
    State { hidden: bool, moving: bool },
    /// The overlay reloaded `config.toml`, which may have been edited elsewhere.
    ConfigChanged,
    /// Bring the window to the front. Handled by [`ControlLink`] itself.
    Activate,
    /// The overlay quits; the window closes too.
    Close,
}

impl ControlEvent {
    pub(super) fn encode(self) -> (usize, isize) {
        match self {
            Self::State { hidden, moving } => (1, isize::from(hidden) | isize::from(moving) << 1),
            Self::ConfigChanged => (2, 0),
            Self::Activate => (3, 0),
            Self::Close => (4, 0),
        }
    }

    fn decode(event: usize, value: isize) -> Option<Self> {
        match event {
            1 => Some(Self::State {
                hidden: value & 1 != 0,
                moving: value & 2 != 0,
            }),
            2 => Some(Self::ConfigChanged),
            3 => Some(Self::Activate),
            4 => Some(Self::Close),
            _ => None,
        }
    }
}

/// Registers a window class with `proc`. A class registered before, also by an earlier call, is fine.
pub(super) fn register_class(name: PCWSTR, proc: WNDPROC) -> Result<(), PlatformError> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc,
        hInstance: instance()?,
        lpszClassName: name,
        ..Default::default()
    };
    // SAFETY: the class structure is fully initialized and its strings are static.
    if unsafe { RegisterClassExW(&class) } == 0 {
        // SAFETY: reads this thread's last error, which RegisterClassExW just set.
        if unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
            return Err(last_error("RegisterClassExW"));
        }
    }
    Ok(())
}

fn post(hwnd: HWND, (command, value): (usize, isize)) -> bool {
    // SAFETY: posting copies two integers into the queue of the window's thread; a window that went away fails.
    unsafe { PostMessageW(Some(hwnd), LINK_MESSAGE, WPARAM(command), LPARAM(value)) }.is_ok()
}

fn find_overlay() -> Option<HWND> {
    // SAFETY: searches top-level windows by a static class name.
    unsafe { FindWindowW(TRAY_CLASS, PCWSTR::null()) }.ok()
}

fn find_control(class: PCWSTR) -> Option<HWND> {
    // SAFETY: searches message-only windows by a static class name.
    unsafe { FindWindowExW(Some(HWND_MESSAGE), None, class, PCWSTR::null()) }.ok()
}

/// Whether an overlay runs in this Windows session.
pub fn overlay_running() -> bool {
    find_overlay().is_some()
}

/// Sends `command` to the running overlay. Returns `false` when no overlay runs.
pub fn send_to_overlay(command: OverlayCommand) -> bool {
    find_overlay().is_some_and(|hwnd| post(hwnd, command.encode()))
}

/// Asks the running overlay to quit, as *Quit Pitot* does (it logs its counters and closes), and waits up to
/// `timeout` for its process to end. Returns `true` once no overlay runs, also when none ran, and `false` when it
/// still runs after `timeout`. Blocks the calling thread; the Pitot window calls it before an update replaces the
/// executable.
pub fn quit_overlay_and_wait(timeout: Duration) -> bool {
    let Some(hwnd) = find_overlay() else {
        return true;
    };
    let mut pid = 0;
    // SAFETY: reads the owner of a window found above into a local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // SAFETY: opens the overlay's process for waiting only, before it is asked to quit, so the id still names it.
    // Failure (an id of 0 from a window that went away) is handled below.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };
    // A window that went away has quit already, or is quitting.
    let _ = post(hwnd, OverlayCommand::Quit.encode());
    match process {
        Ok(handle) => {
            let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
            // SAFETY: waits on the process handle opened above; the wait ends at the timeout at the latest.
            let ended = unsafe { WaitForSingleObject(handle, millis) } == WAIT_OBJECT_0;
            // SAFETY: closes the handle opened above, once.
            let _ = unsafe { CloseHandle(handle) };
            ended
        }
        // Without a handle, the tray window going away is the sign.
        Err(_) => {
            let until = Instant::now() + timeout;
            while overlay_running() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            !overlay_running()
        }
    }
}

/// Tells the Pitot window, if it is open, about `event`. Returns whether it was told.
pub fn notify_control(event: ControlEvent) -> bool {
    notify(CONTROL_CLASS, event)
}

fn notify(class: PCWSTR, event: ControlEvent) -> bool {
    find_control(class).is_some_and(|hwnd| post(hwnd, event.encode()))
}

/// How long a started Pitot window gets to show up before another click starts one again.
const START_GRACE: Duration = Duration::from_secs(3);

thread_local! {
    static STARTED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Brings the Pitot window to the front, or starts it (this executable with [`WINDOW_FLAG`]) when it is not open.
/// Call it in answer to the user (a tray click, a second launch), which lets the window take the focus.
pub fn open_control() -> std::io::Result<()> {
    if activate_control() {
        return Ok(());
    }
    // A double click arrives as two clicks while the first window is still starting.
    if STARTED
        .with(Cell::get)
        .is_some_and(|at| at.elapsed() < START_GRACE)
    {
        return Ok(());
    }
    start_self(Some(WINDOW_FLAG))?;
    STARTED.with(|at| at.set(Some(Instant::now())));
    Ok(())
}

/// Brings an open Pitot window to the front. Returns `false` when none is open. Call it in answer to the user,
/// whose right to take the foreground it passes on to the window's process.
pub fn activate_control() -> bool {
    let Some(hwnd) = find_control(CONTROL_CLASS) else {
        return false;
    };
    let mut pid = 0;
    // SAFETY: reads the owner of a window found above into a local.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    // SAFETY: plain call with a process id; it fails harmlessly when this process may not pass the right on.
    let _ = unsafe { AllowSetForegroundWindow(pid) };
    post(hwnd, ControlEvent::Activate.encode())
}

/// Starts the overlay: this executable again, without arguments. A running overlay makes the new one open the
/// Pitot window and exit.
pub fn start_overlay() -> std::io::Result<()> {
    start_self(None)
}

/// Starts the running executable in a new process, with `arg` if given.
fn start_self(arg: Option<&str>) -> std::io::Result<()> {
    std::process::Command::new(std::env::current_exe()?)
        .args(arg)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

/// What the link thread does with each event.
type OnEvent = Box<dyn FnMut(ControlEvent)>;

thread_local! {
    // The link window and its callback live on the link thread.
    static ON_EVENT: RefCell<Option<OnEvent>> = const { RefCell::new(None) };
}

/// The window to bring to the front on [`ControlEvent::Activate`], as a raw `HWND`.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn link_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        LINK_MESSAGE => {
            match ControlEvent::decode(wparam.0, lparam.0) {
                Some(ControlEvent::Activate) => bring_to_front(),
                Some(event) => ON_EVENT.with(|on_event| {
                    if let Some(on_event) = on_event.borrow_mut().as_mut() {
                        on_event(event);
                    }
                }),
                None => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: destroys this thread's own window, once.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: forwards unchanged arguments for default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn bring_to_front() {
    let hwnd = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
    if hwnd.is_invalid() {
        return;
    }
    // SAFETY: plain window calls on a top-level window of this process; a closed window makes them fail.
    unsafe {
        let show = if IsIconic(hwnd).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOW
        };
        let _ = ShowWindow(hwnd, show);
        let _ = SetForegroundWindow(hwnd);
    }
    // Widgets created since it was last raised, such as by an overlay that just started, are above it.
    ControlLink::keep_above_widgets(true);
}

/// The Pitot window's end of the link: a message-only window on its own thread that receives the overlay's news.
/// Only one may exist per process. Dropping it closes the window and joins the thread.
#[derive(Debug)]
pub struct ControlLink {
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl ControlLink {
    /// Starts the link. `on_event` runs on the link thread for each event but [`ControlEvent::Activate`].
    pub fn start(
        on_event: impl FnMut(ControlEvent) + Send + 'static,
    ) -> Result<Self, PlatformError> {
        Self::start_named(CONTROL_CLASS, on_event)
    }

    fn start_named(
        class: PCWSTR,
        on_event: impl FnMut(ControlEvent) + Send + 'static,
    ) -> Result<Self, PlatformError> {
        // PCWSTR is a pointer; the static name it points to is shared safely as an address.
        let class_name = class.0 as usize;
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("pitot-link".to_owned())
            .spawn(move || {
                let class = PCWSTR(class_name as *const u16);
                let hwnd = match create_link_window(class) {
                    Ok(hwnd) => hwnd,
                    Err(e) => {
                        let _ = sender.send(Err(e));
                        return;
                    }
                };
                ON_EVENT.with(|slot| *slot.borrow_mut() = Some(Box::new(on_event)));
                let _ = sender.send(Ok(hwnd.0 as isize));
                let mut msg = MSG::default();
                // SAFETY: standard message loop over this thread's queue; it ends when the window is destroyed.
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .map_err(|e| PlatformError {
                call: "CreateThread",
                code: e.raw_os_error().unwrap_or(0),
            })?;
        let hwnd = receiver.recv().unwrap_or(Err(PlatformError {
            call: "CreateWindowExW",
            code: 0,
        }))?;
        Ok(Self {
            hwnd,
            thread: Some(thread),
        })
    }

    /// Sets the window of this process that [`ControlEvent::Activate`] brings to the front, as a raw `HWND`.
    pub fn set_window(hwnd: isize) {
        WINDOW.store(hwnd, Ordering::Relaxed);
    }

    /// Keeps the window set by [`ControlLink::set_window`] above the overlay's always-on-top widgets while it is
    /// `active`. Otherwise it goes back among the normal windows, below the window that took the focus.
    pub fn keep_above_widgets(active: bool) {
        let hwnd = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
        if hwnd.is_invalid() {
            return;
        }
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        // SAFETY: z-order changes of a top-level window of this process; a closed window makes them fail.
        unsafe {
            if active {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                return;
            }
            let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
            // Leaving the top of the normal windows would cover the window just activated, such as the sim.
            let foreground = GetForegroundWindow();
            let topmost =
                GetWindowLongPtrW(foreground, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize != 0;
            if !foreground.is_invalid() && foreground != hwnd && !topmost {
                let _ = SetWindowPos(hwnd, Some(foreground), 0, 0, 0, 0, flags);
            }
        }
    }
}

fn create_link_window(class: PCWSTR) -> Result<HWND, PlatformError> {
    register_class(class, Some(link_proc))?;
    // SAFETY: the class is registered and its name is static; a message-only window has no position or size.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Pitot link"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance()?),
            None,
        )
    }
    .map_err(|e| error("CreateWindowExW", &e))
}

impl Drop for ControlLink {
    fn drop(&mut self) {
        // SAFETY: asks the link thread's window to close, which ends its message loop.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        for command in [
            OverlayCommand::Hello,
            OverlayCommand::SetHidden(true),
            OverlayCommand::SetHidden(false),
            OverlayCommand::SetMoving(true),
            OverlayCommand::Quit,
            OverlayCommand::BackgroundPreview(0),
            OverlayCommand::BackgroundPreview(100),
            OverlayCommand::BackgroundSaved(37),
            OverlayCommand::TextSizePreview(70),
            OverlayCommand::TextSizePreview(150),
            OverlayCommand::TextSizeSaved(115),
        ] {
            let (c, v) = command.encode();
            assert_eq!(OverlayCommand::decode(c, v), Some(command));
        }
        // A percent out of range from another sender is clamped.
        assert_eq!(
            OverlayCommand::decode(5, 1000),
            Some(OverlayCommand::BackgroundPreview(100))
        );
        assert_eq!(
            OverlayCommand::decode(6, -5),
            Some(OverlayCommand::BackgroundSaved(0))
        );
        assert_eq!(
            OverlayCommand::decode(7, 1000),
            Some(OverlayCommand::TextSizePreview(150))
        );
        assert_eq!(
            OverlayCommand::decode(8, 0),
            Some(OverlayCommand::TextSizeSaved(70))
        );
        for event in [
            ControlEvent::State {
                hidden: true,
                moving: false,
            },
            ControlEvent::State {
                hidden: false,
                moving: true,
            },
            ControlEvent::ConfigChanged,
            ControlEvent::Activate,
            ControlEvent::Close,
        ] {
            let (e, v) = event.encode();
            assert_eq!(ControlEvent::decode(e, v), Some(event));
        }
        assert_eq!(OverlayCommand::decode(99, 0), None);
        assert_eq!(ControlEvent::decode(0, 0), None);
    }

    #[test]
    fn the_link_receives_events_in_order() {
        // A class of its own, so a Pitot window open on this machine is not involved.
        let class = w!("PitotControlLinkTest");
        let (sender, receiver) = std::sync::mpsc::channel();
        let link = ControlLink::start_named(class, move |event| {
            let _ = sender.send(event);
        })
        .expect("link");
        let state = ControlEvent::State {
            hidden: true,
            moving: false,
        };
        assert!(notify(class, state));
        assert!(notify(class, ControlEvent::ConfigChanged));
        assert!(notify(class, ControlEvent::Close));
        let wait = Duration::from_secs(5);
        assert_eq!(receiver.recv_timeout(wait), Ok(state));
        assert_eq!(receiver.recv_timeout(wait), Ok(ControlEvent::ConfigChanged));
        assert_eq!(receiver.recv_timeout(wait), Ok(ControlEvent::Close));
        drop(link);
        assert!(!notify(class, ControlEvent::Close), "gone after drop");
    }
}
