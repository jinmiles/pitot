//! Win32 calls without a safe wrapper: process enumeration, per-process times and memory, closing an app's
//! windows, process termination, job objects, PDH counters, the clock, and registry strings. All of this crate's `unsafe` lives in this module.

use std::fmt;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, FILETIME, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTimeAsFileTime};
use windows::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetCurrentProcess, GetCurrentThread,
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE, PROCESS_VM_READ, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::System::WindowsProgramming::{QueryProcessCycleTime, QueryThreadCycleTime};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
};
use windows::core::{BOOL, HSTRING, PCWSTR};

use crate::collect::ProcessEntry;

/// A failed Win32 or PDH call and its error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysError {
    pub call: &'static str,
    pub code: u32,
}

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed (0x{:08X})", self.call, self.code)
    }
}

impl std::error::Error for SysError {}

fn win(call: &'static str) -> impl Fn(windows::core::Error) -> SysError {
    move |e| SysError {
        call,
        code: e.code().0 as u32,
    }
}

fn pdh(call: &'static str, status: u32) -> Result<(), SysError> {
    if status == 0 {
        Ok(())
    } else {
        Err(SysError { call, code: status })
    }
}

fn filetime(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// Fills `out` with every running process: PID, parent PID, and executable name.
pub fn processes(out: &mut Vec<ProcessEntry>) -> Result<(), SysError> {
    out.clear();
    // SAFETY: takes a process snapshot; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(win("CreateToolhelp32Snapshot"))?;
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `entry` is a PROCESSENTRY32W with `dwSize` set, for the snapshot taken above.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let name = &entry.szExeFile;
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        out.push(ProcessEntry {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&name[..len]),
        });
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    // SAFETY: closes the snapshot handle created above, once.
    let _ = unsafe { CloseHandle(snapshot) };
    Ok(())
}

/// Creation time and total CPU time (kernel plus user), both in 100 ns units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessTimes {
    pub created: u64,
    pub cpu: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessMemory {
    pub private_bytes: u64,
    pub working_set: u64,
}

fn times_of(handle: HANDLE) -> Result<ProcessTimes, SysError> {
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    // SAFETY: `handle` has query rights; the four FILETIMEs are valid out pointers.
    unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
        .map_err(win("GetProcessTimes"))?;
    Ok(ProcessTimes {
        created: filetime(created),
        cpu: filetime(kernel) + filetime(user),
    })
}

/// A process handle with query, memory-read, and wait rights. An exited process keeps its final times readable
/// while the handle is open.
#[derive(Debug)]
pub struct Process {
    handle: HANDLE,
}

impl Process {
    pub fn open(pid: u32) -> Result<Self, SysError> {
        // SAFETY: opens a handle that is closed in Drop.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }
        .map_err(win("OpenProcess"))?;
        Ok(Self { handle })
    }

    pub fn times(&self) -> Result<ProcessTimes, SysError> {
        times_of(self.handle)
    }

    /// CPU cycles of all threads, including exited ones. Not tied to the clock tick, unlike the times.
    pub fn cycles(&self) -> Result<u64, SysError> {
        let mut cycles = 0;
        // SAFETY: `handle` has query rights; `cycles` is a valid out pointer.
        unsafe { QueryProcessCycleTime(self.handle, &mut cycles) }
            .map_err(win("QueryProcessCycleTime"))?;
        Ok(cycles)
    }

    pub fn memory(&self) -> Result<ProcessMemory, SysError> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: the EX structure starts with PROCESS_MEMORY_COUNTERS, and `cb` gives the call its full size.
        unsafe {
            GetProcessMemoryInfo(
                self.handle,
                (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        }
        .map_err(win("GetProcessMemoryInfo"))?;
        Ok(ProcessMemory {
            private_bytes: counters.PrivateUsage as u64,
            working_set: counters.WorkingSetSize as u64,
        })
    }

    pub fn exited(&self) -> bool {
        // SAFETY: `handle` has wait rights; a zero timeout only polls.
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: closes the handle owned by `self`, once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Terminates a process if it is still the one created at `created` (a FILETIME value), so a reused PID is left
/// alone. Returns whether it was terminated.
pub fn terminate(pid: u32, created: u64) -> Result<bool, SysError> {
    // SAFETY: opens a handle that is closed below.
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
    }
    .map_err(win("OpenProcess"))?;
    let result = times_of(handle).and_then(|times| {
        if times.created != created {
            return Ok(false);
        }
        // SAFETY: `handle` has terminate rights.
        unsafe { TerminateProcess(handle, 1) }.map_err(win("TerminateProcess"))?;
        Ok(true)
    });
    // SAFETY: closes the handle opened above, once.
    let _ = unsafe { CloseHandle(handle) };
    result
}

/// A job object that terminates its processes when it is closed, so processes a series started do not outlive
/// it. Processes in the job may still start children outside it (breakaway is allowed), which keeps apps that ask
/// for breakaway working.
#[derive(Debug)]
pub struct Job {
    handle: HANDLE,
}

impl Job {
    pub fn new() -> Result<Self, SysError> {
        // SAFETY: creates an unnamed job; the handle is owned by the returned value and closed in Drop.
        let handle =
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(win("CreateJobObjectW"))?;
        let job = Self { handle };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        // SAFETY: `limits` is the structure this information class expects, with its exact size.
        unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(win("SetInformationJobObject"))?;
        Ok(job)
    }

    /// Adds a child process. Processes it starts from then on join the job too.
    pub fn assign(&self, child: &std::process::Child) -> Result<(), SysError> {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the child's handle stays open while `child` is borrowed and has the rights CreateProcess grants.
        unsafe { AssignProcessToJobObject(self.handle, HANDLE(child.as_raw_handle())) }
            .map_err(win("AssignProcessToJobObject"))
    }

    /// Terminates every process in the job.
    pub fn terminate(&self) -> Result<(), SysError> {
        // SAFETY: the job handle is open and has all access.
        unsafe { TerminateJobObject(self.handle, 1) }.map_err(win("TerminateJobObject"))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: closes the job handle owned by `self`, once; this terminates processes still in the job.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "system" fn on_console_control(kind: u32) -> windows::core::BOOL {
    if kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT {
        STOP.store(true, std::sync::atomic::Ordering::Relaxed);
        return true.into();
    }
    false.into()
}

/// Makes Ctrl+C and Ctrl+Break set the returned flag instead of ending the process, so a long task can finish
/// its files.
pub fn stop_on_ctrl_c() -> Result<&'static std::sync::atomic::AtomicBool, SysError> {
    // SAFETY: the handler is a plain function that only stores to a static atomic; it stays valid for the
    // process's lifetime.
    unsafe { SetConsoleCtrlHandler(Some(on_console_control), true) }
        .map_err(win("SetConsoleCtrlHandler"))?;
    Ok(&STOP)
}

/// Posts `WM_CLOSE` to every top-level window, shown or hidden, of the processes in `pids`, as closing an app's
/// windows does. Returns how many windows were asked.
pub fn close_windows(pids: &[u32]) -> usize {
    struct Search<'a> {
        pids: &'a [u32],
        asked: usize,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Search` that `close_windows` passed; it outlives the enumeration and nothing
        // else uses it meanwhile.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0;
        // SAFETY: `hwnd` comes from the enumeration; `pid` is a valid out pointer.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // SAFETY: posting a message touches no memory of this process; a window that is gone fails the call.
        if search.pids.contains(&pid)
            && unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok()
        {
            search.asked += 1;
        }
        true.into()
    }
    let mut search = Search { pids, asked: 0 };
    // SAFETY: the callback runs only during the call and gets a pointer to `search`, which lives on this frame.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&raw mut search as isize)) };
    search.asked
}

/// CPU time of this process, in 100 ns units.
pub fn current_process_cpu() -> Result<u64, SysError> {
    // SAFETY: the pseudo handle for the current process is always valid and needs no closing.
    times_of(unsafe { GetCurrentProcess() }).map(|t| t.cpu)
}

/// CPU cycles of this process, including exited threads.
pub fn current_process_cycles() -> Result<u64, SysError> {
    let mut cycles = 0;
    // SAFETY: the pseudo handle for the current process is always valid; `cycles` is a valid out pointer.
    unsafe { QueryProcessCycleTime(GetCurrentProcess(), &mut cycles) }
        .map_err(win("QueryProcessCycleTime"))?;
    Ok(cycles)
}

/// CPU cycles the calling thread has used.
pub fn current_thread_cycles() -> Result<u64, SysError> {
    let mut cycles = 0;
    // SAFETY: the pseudo handle for the current thread is always valid; `cycles` is a valid out pointer.
    unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) }
        .map_err(win("QueryThreadCycleTime"))?;
    Ok(cycles)
}

/// Logical processors across all processor groups.
pub fn logical_processors() -> u32 {
    // SAFETY: no pointers; returns 0 only on failure.
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
}

/// The system clock as a FILETIME value (100 ns units since 1601), comparable with process creation times.
pub fn now_filetime() -> u64 {
    // SAFETY: no pointers; cannot fail.
    filetime(unsafe { GetSystemTimeAsFileTime() })
}

/// Local wall-clock time as year, month, day, hour, minute, second.
pub fn local_time() -> [u16; 6] {
    // SAFETY: GetLocalTime only fills the SYSTEMTIME it returns and cannot fail.
    let t = unsafe { GetLocalTime() };
    [t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond]
}

/// A string value under `HKEY_LOCAL_MACHINE`.
pub fn registry_string(subkey: &str, value: &str) -> Option<String> {
    let (subkey, value) = (HSTRING::from(subkey), HSTRING::from(value));
    let mut size = 0u32;
    // SAFETY: asks for the size only; the strings outlive the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: `buffer` holds `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]).trim().to_owned())
}

/// A DWORD value under `HKEY_LOCAL_MACHINE`.
pub fn registry_dword(subkey: &str, value: &str) -> Option<u32> {
    let (subkey, value) = (HSTRING::from(subkey), HSTRING::from(value));
    let (mut data, mut size) = (0u32, size_of::<u32>() as u32);
    // SAFETY: `data` holds the four bytes announced in `size`.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

/// Doubles, and percentages above 100 (a process can use more than one core).
const FORMAT: PDH_FMT = PDH_FMT(PDH_FMT_DOUBLE.0 | 0x8000);

fn valid(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

/// A counter in a [`PdhQuery`].
#[derive(Debug, Clone, Copy)]
pub struct Counter(PDH_HCOUNTER);

/// A real-time PDH query. Rate counters have values from the second collection on.
pub struct PdhQuery {
    query: PDH_HQUERY,
    items: Vec<PDH_FMT_COUNTERVALUE_ITEM_W>,
    name: String,
}

impl PdhQuery {
    pub fn open() -> Result<Self, SysError> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: opens a real-time query, closed in Drop.
        pdh("PdhOpenQueryW", unsafe {
            PdhOpenQueryW(PCWSTR::null(), 0, &mut query)
        })?;
        Ok(Self {
            query,
            items: Vec::new(),
            name: String::new(),
        })
    }

    /// Adds a counter by its English path, so localized Windows works. A `*` instance follows instances as they
    /// come and go.
    pub fn add(&mut self, path: &str) -> Result<Counter, SysError> {
        let path = HSTRING::from(path);
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: `path` outlives the call; the counter belongs to the query and is freed with it.
        pdh("PdhAddEnglishCounterW", unsafe {
            PdhAddEnglishCounterW(self.query, &path, 0, &mut counter)
        })?;
        Ok(Counter(counter))
    }

    pub fn collect(&mut self) -> Result<(), SysError> {
        // SAFETY: the query handle is open.
        pdh("PdhCollectQueryData", unsafe {
            PdhCollectQueryData(self.query)
        })
    }

    /// The value of a single-instance counter, when valid.
    pub fn value(&self, counter: Counter) -> Option<f64> {
        let mut value = PDH_FMT_COUNTERVALUE::default();
        // SAFETY: `value` is a valid out structure for the counter's formatted value.
        let status = unsafe { PdhGetFormattedCounterValue(counter.0, FORMAT, None, &mut value) };
        if status != 0 || !valid(value.CStatus) {
            return None;
        }
        // SAFETY: with PDH_FMT_DOUBLE the union holds `doubleValue`.
        Some(unsafe { value.Anonymous.doubleValue })
    }

    /// Calls `f` with the instance name and value of every valid instance of a wildcard counter.
    pub fn each(&mut self, counter: Counter, mut f: impl FnMut(&str, f64)) -> Result<(), SysError> {
        let item = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let mut count = 0u32;
        // Instances can appear between the size query and the read; retry a few times.
        for _ in 0..4 {
            let mut size = (self.items.len() * item) as u32;
            let buffer = (!self.items.is_empty()).then_some(self.items.as_mut_ptr());
            // SAFETY: `items` holds `size` bytes, aligned for the item structure (no buffer when empty, which only
            // asks for the size); the names PDH writes after the items stay inside that buffer.
            let status = unsafe {
                PdhGetFormattedCounterArrayW(counter.0, FORMAT, &mut size, &mut count, buffer)
            };
            if status == PDH_MORE_DATA {
                self.items
                    .resize_with((size as usize).div_ceil(item) + 16, Default::default);
                continue;
            }
            pdh("PdhGetFormattedCounterArrayW", status)?;
            for entry in &self.items[..count as usize] {
                if !valid(entry.FmtValue.CStatus) {
                    continue;
                }
                // SAFETY: PDH set `szName` to a NUL-terminated string inside `items`, valid until the next call.
                let wide = unsafe { entry.szName.as_wide() };
                self.name.clear();
                self.name.extend(
                    char::decode_utf16(wide.iter().copied())
                        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)),
                );
                // SAFETY: with PDH_FMT_DOUBLE the union holds `doubleValue`.
                f(&self.name, unsafe { entry.FmtValue.Anonymous.doubleValue });
            }
            return Ok(());
        }
        Err(SysError {
            call: "PdhGetFormattedCounterArrayW",
            code: PDH_MORE_DATA,
        })
    }
}

impl fmt::Debug for PdhQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PdhQuery").finish_non_exhaustive()
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        // SAFETY: closes the query owned by `self`, once, which also frees its counters.
        let _ = unsafe { PdhCloseQuery(self.query) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn closes_nothing_without_windows() {
        // PID 0 is the idle process, which has no windows.
        assert_eq!(super::close_windows(&[0]), 0);
        assert_eq!(super::close_windows(&[]), 0);
    }
}
