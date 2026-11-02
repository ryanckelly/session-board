//! Process liveness. A registry or roster file outlives a crash, so a pid alone proves little: on
//! Linux and Windows its start time must match too (a reused pid would not). Without `/proc`
//! (macOS) the pid existing has to do.
//!
//! `procStart` is whatever Claude Code reads for the platform: field 22 of `/proc/<pid>/stat` on
//! Linux, the creation time from `GetProcessTimes` (a FILETIME, 100 ns ticks since 1601) on
//! Windows.

use serde_json::Value;
#[cfg(unix)]
use std::path::Path;

/// Is the process `pid`, started at `proc_start`, still running?
#[cfg(unix)]
pub fn alive(pid: Option<&Value>, proc_start: Option<&Value>) -> bool {
    alive_with(pid, proc_start, Path::new("/proc/self").is_dir())
}

/// Is the process `pid`, started at `proc_start`, still running?
#[cfg(windows)]
pub fn alive(pid: Option<&Value>, proc_start: Option<&Value>) -> bool {
    let Some(pid) = pid.and_then(as_pid) else { return false };
    win::creation_time(pid as u32).is_some_and(|start| start.to_string() == as_text(proc_start))
}

/// This process's own start time, for tests that need a live roster entry.
#[cfg(windows)]
pub fn own_start_time() -> Option<String> {
    win::creation_time(std::process::id()).map(|t| t.to_string())
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// The creation time of running process `pid`, as Claude Code writes `procStart`; `None` when
    /// there is no such process, it has exited, or it can't be queried.
    pub fn creation_time(pid: u32) -> Option<u64> {
        // SAFETY: plain Win32 calls on a handle we open and close here; the out-pointers are to
        // locals that live for the whole call.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
            let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
            let mut code = 0u32;
            // A process that has exited can still be opened while anyone holds a handle to it.
            let running = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
            let ok = GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) != 0;
            CloseHandle(h);
            (running && ok).then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
        }
    }
}

/// [`alive`], told whether `/proc` is there (tests use this to check the macOS path).
#[cfg(unix)]
pub fn alive_with(pid: Option<&Value>, proc_start: Option<&Value>, has_proc: bool) -> bool {
    let Some(pid) = pid.and_then(as_pid) else { return false };
    if !has_proc {
        // SAFETY: signal 0 only checks the pid exists and may be signalled; nothing is sent.
        let r = unsafe { libc::kill(pid, 0) };
        // EPERM: it exists, owned by someone else.
        return r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    start_time(&stat).is_some_and(|start| start == as_text(proc_start))
}

/// Field 22 of `/proc/<pid>/stat`: the start time, counted after the parenthesised command name
/// (which may itself contain spaces and parentheses).
pub fn start_time(stat: &str) -> Option<&str> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)
}

/// This process's own start time, for tests that need a live roster entry.
#[cfg(unix)]
pub fn own_start_time() -> Option<String> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    start_time(&stat).map(String::from)
}

fn as_pid(v: &Value) -> Option<i32> {
    let n = match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64))?,
        Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    i32::try_from(n).ok().filter(|p| *p > 0)
}

/// `procStart` as text: Claude Code writes it as a string, but a number reads the same.
fn as_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}
