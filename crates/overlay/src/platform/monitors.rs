//! The connected monitors in Pitot's order, which the overlay places widgets by and the Pitot window lists. Both
//! processes call [`monitors`], so they number the monitors the same way.

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use super::ScreenRect;
use crate::config;

/// A connected monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    pub rect: ScreenRect,
    pub primary: bool,
    /// Effective DPI (96 at 100% scaling).
    pub dpi: u32,
    /// The name Windows gives the monitor's output, such as `\\.\DISPLAY1`. It can change when monitors are
    /// connected again; the order of [`monitors`] does not follow it.
    pub device: String,
    /// The monitor's own name, such as `DELL U2720Q`, from [`displays`] where Windows provides one (from the
    /// monitor's EDID). [`monitors`] leaves it out.
    pub name: Option<String>,
}

/// Lists the connected monitors in Pitot's order: left to right, then top to bottom, by their top-left corner
/// ([`config::monitor_order`]). The `monitor` settings count in this order.
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
                device: wide(&info.szDevice),
                name: None,
            });
        }
        true.into()
    }
    let mut found: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only runs during this call and receives a pointer to `found`, which outlives it.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut found as *mut _ as isize),
        )
    };
    let corners: Vec<(i32, i32)> = found.iter().map(|m| (m.rect.x, m.rect.y)).collect();
    let mut slots: Vec<Option<Monitor>> = found.into_iter().map(Some).collect();
    config::monitor_order(&corners)
        .into_iter()
        .filter_map(|index| slots.get_mut(index).and_then(Option::take))
        .collect()
}

/// [`monitors`] with their own names where Windows provides them, for the Pitot window's list.
pub fn displays() -> Vec<Monitor> {
    let names = friendly_names();
    let mut list = monitors();
    for monitor in &mut list {
        monitor.name = names
            .iter()
            .find(|(device, _)| device.eq_ignore_ascii_case(&monitor.device))
            .map(|(_, name)| name.clone());
    }
    list
}

/// Each active output's name (`\\.\DISPLAY1`) with the name of the monitor on it, for the outputs whose monitor
/// has one. A monitor duplicated on several outputs is listed for each. Empty when Windows cannot say.
fn friendly_names() -> Vec<(String, String)> {
    // The configuration can change between asking for the sizes and reading it; then the sizes are asked again.
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        // SAFETY: writes two counts into locals.
        let sized = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if sized != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: the arrays hold as many elements as the counts say, and the call writes no more than that.
        let read = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if read == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if read != ERROR_SUCCESS {
            return Vec::new();
        }
        paths.truncate(path_count as usize);
        return paths.iter().filter_map(path_names).collect();
    }
    Vec::new()
}

/// The output's name and its monitor's name for one display path, when Windows has both.
fn path_names(path: &DISPLAYCONFIG_PATH_INFO) -> Option<(String, String)> {
    let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: the header starts a DISPLAYCONFIG_SOURCE_DEVICE_NAME whose size it states, which the call fills.
    if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
        return None;
    }
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: the header starts a DISPLAYCONFIG_TARGET_DEVICE_NAME whose size it states, which the call fills.
    if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0 {
        return None;
    }
    let name = wide(&target.monitorFriendlyDeviceName);
    let name = name.trim();
    (!name.is_empty()).then(|| (wide(&source.viewGdiDeviceName), name.to_owned()))
}

/// A NUL-terminated UTF-16 buffer as text.
fn wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitors_come_in_pitots_order_with_the_names_beside_them() {
        let listed = monitors();
        let corners: Vec<(i32, i32)> = listed.iter().map(|m| (m.rect.x, m.rect.y)).collect();
        let order = config::monitor_order(&corners);
        assert_eq!(order, (0..listed.len()).collect::<Vec<_>>(), "sorted");
        assert!(listed.iter().filter(|m| m.primary).count() <= 1);
        assert!(listed.iter().all(|m| m.name.is_none()));
        // The window's list: the same monitors in the same order, some with names.
        let named = displays();
        let strip = |m: &Monitor| Monitor {
            name: None,
            ..m.clone()
        };
        assert_eq!(named.iter().map(strip).collect::<Vec<_>>(), listed);
    }

    #[test]
    fn wide_text_ends_at_the_first_nul() {
        let text: Vec<u16> = "DELL U2720Q\0junk".encode_utf16().collect();
        assert_eq!(wide(&text), "DELL U2720Q");
        assert_eq!(wide(&[]), "");
        let full: Vec<u16> = "ABC".encode_utf16().collect();
        assert_eq!(wide(&full), "ABC", "no NUL");
    }
}
