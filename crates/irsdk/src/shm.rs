//! Owners of Win32 file-mapping views and events. All shared-memory `unsafe` in the crate lives here.

use core::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_BASIC_INFORMATION,
    MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
    VirtualQuery,
};
use windows::Win32::System::Threading::{
    CreateEventW, OpenEventW, SYNCHRONIZATION_SYNCHRONIZE, SetEvent, WaitForSingleObject,
};
use windows::core::HSTRING;

use crate::live::LiveError;

fn os_error(call: &'static str, error: &windows::core::Error) -> LiveError {
    LiveError::Os {
        call,
        code: error.code().0,
    }
}

fn is_not_found(error: &windows::core::Error) -> bool {
    error.code() == ERROR_FILE_NOT_FOUND.to_hresult()
}

fn close(handle: HANDLE) {
    // SAFETY: `handle` is owned by the caller and closed exactly once; a failure leaves nothing to clean up.
    let _ = unsafe { CloseHandle(handle) };
}

fn last_error_is_already_exists() -> bool {
    // SAFETY: reads the calling thread's last-error value; called right after the Win32 call that set it.
    unsafe { GetLastError() == ERROR_ALREADY_EXISTS }
}

/// A mapped view of a named file mapping.
#[derive(Debug)]
pub(crate) struct Mapping {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    writable: bool,
}

impl Mapping {
    /// Opens an existing mapping read-only. `Ok(None)` when no mapping has that name.
    pub(crate) fn open_read(name: &HSTRING) -> Result<Option<Self>, LiveError> {
        // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
        match unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, name) } {
            Ok(handle) => Self::map(handle, FILE_MAP_READ, false).map(Some),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(os_error("OpenFileMappingW", &e)),
        }
    }

    /// Creates a new pagefile-backed mapping of at least `size` bytes. Fails if the name is already in use.
    pub(crate) fn create(name: &HSTRING, size: usize) -> Result<Self, LiveError> {
        let size = size as u64;
        // SAFETY: INVALID_HANDLE_VALUE requests a pagefile-backed section; `name` outlives the call.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                (size >> 32) as u32,
                size as u32,
                name,
            )
        }
        .map_err(|e| os_error("CreateFileMappingW", &e))?;
        if last_error_is_already_exists() {
            close(handle);
            return Err(LiveError::AlreadyExists);
        }
        Self::map(handle, FILE_MAP_ALL_ACCESS, true)
    }

    fn map(handle: HANDLE, access: FILE_MAP, writable: bool) -> Result<Self, LiveError> {
        // SAFETY: `handle` is a valid file-mapping handle owned here; offset 0 and length 0 map the whole section.
        let view = unsafe { MapViewOfFile(handle, access, 0, 0, 0) };
        if view.Value.is_null() {
            let error = windows::core::Error::from_thread();
            close(handle);
            return Err(os_error("MapViewOfFile", &error));
        }
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: `view` is the base of the view mapped above and `info` is a writable buffer of the given size.
        let written = unsafe {
            VirtualQuery(
                Some(view.Value),
                &mut info,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        let mapping = Self {
            handle,
            view,
            len: info.RegionSize,
            writable,
        };
        if written == 0 {
            return Err(os_error(
                "VirtualQuery",
                &windows::core::Error::from_thread(),
            ));
        }
        Ok(mapping)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    fn base(&self) -> *mut u8 {
        self.view.Value.cast()
    }

    fn in_view(&self, offset: usize, len: usize) -> bool {
        offset.checked_add(len).is_some_and(|end| end <= self.len)
    }

    /// Copies `out.len()` bytes starting at `offset`. Returns `false` when the range is outside the view.
    pub(crate) fn read(&self, offset: usize, out: &mut [u8]) -> bool {
        if !self.in_view(offset, out.len()) {
            return false;
        }
        // SAFETY: `offset..offset + out.len()` lies inside the view, which stays mapped while `self` lives, and
        // `out` is a separate buffer. Another process may write the view concurrently; callers detect torn
        // copies through the buffer tick count, so no reference into the view is ever formed.
        unsafe {
            core::ptr::copy_nonoverlapping(self.base().add(offset), out.as_mut_ptr(), out.len())
        };
        true
    }

    /// Writes `data` at `offset`. Returns `false` for read-only views or ranges outside the view.
    pub(crate) fn write(&self, offset: usize, data: &[u8]) -> bool {
        if !self.writable || !self.in_view(offset, data.len()) {
            return false;
        }
        // SAFETY: the view is writable and `offset..offset + data.len()` lies inside it; `data` is a separate
        // buffer. Readers in other processes treat the bytes as untrusted until the tick count is published.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.base().add(offset), data.len())
        };
        true
    }

    fn atomic(&self, offset: usize) -> Option<&AtomicI32> {
        if !offset.is_multiple_of(4) || !self.in_view(offset, 4) {
            return None;
        }
        // SAFETY: the view base is allocation-granularity aligned, so a 4-byte aligned offset gives an aligned
        // pointer inside the view. The view outlives the returned borrow, and every access to these words goes
        // through atomics (the sim's aligned 4-byte stores are atomic on x86-64).
        Some(unsafe { AtomicI32::from_ptr(self.base().add(offset).cast()) })
    }

    /// Acquire load of an aligned 4-byte value, such as a buffer tick count.
    pub(crate) fn load_i32(&self, offset: usize) -> Option<i32> {
        self.atomic(offset).map(|a| a.load(Ordering::Acquire))
    }

    /// Release store of an aligned 4-byte value. Returns `false` for read-only views or invalid offsets.
    pub(crate) fn store_i32(&self, offset: usize, value: i32) -> bool {
        match self.atomic(offset) {
            Some(a) if self.writable => {
                a.store(value, Ordering::Release);
                true
            }
            _ => false,
        }
    }
}

// SAFETY: the mapping handle and view are process-wide resources that any thread may use or release; all
// access to the view goes through bounds-checked copies or atomics, so moving the owner between threads is sound.
unsafe impl Send for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `view` came from MapViewOfFile and is unmapped exactly once, before its handle is closed.
        let _ = unsafe { UnmapViewOfFile(self.view) };
        close(self.handle);
    }
}

/// A named Win32 event.
#[derive(Debug)]
pub(crate) struct Event {
    handle: HANDLE,
}

impl Event {
    /// Opens an existing event for waiting. `Ok(None)` when no event has that name.
    pub(crate) fn open(name: &HSTRING) -> Result<Option<Self>, LiveError> {
        // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
        match unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, name) } {
            Ok(handle) => Ok(Some(Self { handle })),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(os_error("OpenEventW", &e)),
        }
    }

    /// Creates a new auto-reset event. Fails if the name is already in use.
    pub(crate) fn create(name: &HSTRING) -> Result<Self, LiveError> {
        // SAFETY: default security, auto-reset, initially unsignaled; `name` outlives the call.
        let handle = unsafe { CreateEventW(None, false, false, name) }
            .map_err(|e| os_error("CreateEventW", &e))?;
        if last_error_is_already_exists() {
            close(handle);
            return Err(LiveError::AlreadyExists);
        }
        Ok(Self { handle })
    }

    /// Waits up to `timeout`. Returns `true` when the event was signaled.
    pub(crate) fn wait(&self, timeout: Duration) -> Result<bool, LiveError> {
        // INFINITE is u32::MAX; cap below it so a long timeout never blocks forever.
        let millis = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX - 1)
            .min(u32::MAX - 1);
        // SAFETY: `handle` is a valid event handle owned by `self`.
        match unsafe { WaitForSingleObject(self.handle, millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(os_error(
                "WaitForSingleObject",
                &windows::core::Error::from_thread(),
            )),
        }
    }

    pub(crate) fn set(&self) -> Result<(), LiveError> {
        // SAFETY: `handle` is a valid event handle owned by `self`.
        unsafe { SetEvent(self.handle) }.map_err(|e| os_error("SetEvent", &e))
    }
}

// SAFETY: event handles are process-wide and may be waited on, signaled, or closed from any thread.
unsafe impl Send for Event {}

impl Drop for Event {
    fn drop(&mut self) {
        close(self.handle);
    }
}
