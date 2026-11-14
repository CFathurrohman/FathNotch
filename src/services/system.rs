//! System monitor and the Apps (task manager) list: CPU, memory, disk, network and battery, plus running
//! programs grouped by name like the .NET Task Manager view. CPU and network rates are deltas between
//! successive calls, so the first call reports 0 for them; poll every 1–2 s while the page is on screen.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, FILETIME, HANDLE, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::ProcessStatus::{EnumProcesses, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetProcessTimes, GetSystemTimes, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongW, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE, GW_OWNER,
    WS_EX_TOOLWINDOW,
};

#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Whole-machine CPU use, 0–100.
    pub cpu: f32,
    pub ram_used: u64,
    pub ram_total: u64,
    /// Bytes per second over all connected adapters (loopback and tunnels left out).
    pub net_down: u64,
    pub net_up: u64,
    /// (percent, plugged in); None on a PC without a battery.
    pub battery: Option<(u8, bool)>,
    /// Battery time left in seconds while discharging, when Windows knows it.
    pub battery_secs: Option<u32>,
    /// System drive (used, total) bytes.
    pub disk: Option<(u64, u64)>,
}

struct Prev {
    idle: u64,
    busy: u64,
    rx: u64,
    tx: u64,
    at: Instant,
}

static PREV: Mutex<Option<Prev>> = Mutex::new(None);

fn ft(f: FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

pub fn sample() -> Stats {
    let mut s = Stats::default();
    let now = Instant::now();
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    let times = unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_ok();
    // Kernel time includes idle time.
    let (idle, busy) = (ft(idle), ft(kernel) + ft(user));
    let (rx, tx) = net_totals().unwrap_or((0, 0));

    {
        let mut prev = PREV.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = prev.as_ref() {
            let total = busy.saturating_sub(p.busy);
            if times && total > 0 {
                let idle_d = idle.saturating_sub(p.idle);
                s.cpu = (100.0 * total.saturating_sub(idle_d) as f64 / total as f64).clamp(0.0, 100.0) as f32;
            }
            let secs = now.duration_since(p.at).as_secs_f64().max(0.1);
            s.net_down = (rx.saturating_sub(p.rx) as f64 / secs) as u64;
            s.net_up = (tx.saturating_sub(p.tx) as f64 / secs) as u64;
        }
        *prev = Some(Prev { idle, busy, rx, tx, at: now });
    }

    let mut mem = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
        s.ram_total = mem.ullTotalPhys;
        s.ram_used = mem.ullTotalPhys - mem.ullAvailPhys;
    }

    let mut power = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut power) }.is_ok() && power.BatteryFlag & 128 == 0 && power.BatteryFlag != 255 && power.BatteryLifePercent <= 100 {
        let charging = power.ACLineStatus == 1;
        s.battery = Some((power.BatteryLifePercent, charging));
        if !charging && power.BatteryLifeTime != u32::MAX {
            s.battery_secs = Some(power.BatteryLifeTime);
        }
    }

    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()) + "\\";
    let (mut total, mut free) = (0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(&HSTRING::from(drive), None, Some(&mut total), Some(&mut free)) }.is_ok() && total > 0 {
        s.disk = Some((total - free, total));
    }
    s
}

/// Received/sent byte counters summed over connected, real adapters.
fn net_totals() -> Option<(u64, u64)> {
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return None;
        }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let (mut rx, mut tx) = (0u64, 0u64);
        for r in rows {
            let filter = r.InterfaceAndOperStatusFlags._bitfield & 0b10 != 0; // NDIS filter layers repeat the adapter's counters
            if r.OperStatus != IfOperStatusUp || filter || r.Type == IF_TYPE_SOFTWARE_LOOPBACK || r.Type == IF_TYPE_TUNNEL {
                continue;
            }
            rx += r.InOctets;
            tx += r.OutOctets;
        }
        FreeMibTable(table as _);
        Some((rx, tx))
    }
}

/// "1.2 MB/s" — at most "00.0 MB/s" wide, like the .NET idle notch.
pub fn rate(bytes_per_sec: u64) -> String {
    let b = bytes_per_sec as f64;
    if b >= 100.0 * 1024.0 * 1024.0 {
        format!("{:.0} MB/s", b / 1048576.0)
    } else if b >= 1000.0 * 1024.0 {
        format!("{:.1} MB/s", b / 1048576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB/s", b / 1024.0)
    } else {
        format!("{b:.0} B/s")
    }
}

// ---------------------------------------------------------------- processes

/// One program: every process with the same executable name, grouped (all of Chrome is one row).
#[derive(Clone, Debug, Default)]
pub struct Proc {
    /// The first process of the group (its icon / description come from this one).
    pub pid: u32,
    /// "chrome" (process name without .exe).
    pub name: String,
    /// Friendly name from the exe's version info ("Google Chrome"), else `name`.
    pub title: String,
    /// Share of all cores since the previous call, 0–100 (0 on the first call).
    pub cpu: f32,
    /// Private working set in bytes, summed over the group.
    pub mem: u64,
    /// Some process of the group has a visible top-level window (an "app" rather than background work).
    pub has_window: bool,
    pub pids: Vec<u32>,
    /// Full path of `pid`'s executable.
    pub path: String,
}

struct ProcPrev {
    /// (pid, creation time) → kernel+user time, in 100 ns units.
    cpu: HashMap<(u32, u64), u64>,
    at: Instant,
}

static PROC_PREV: Mutex<Option<ProcPrev>> = Mutex::new(None);
static DESCRIPTIONS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Every process this account can read (system services and elevated apps are left out, like the .NET
/// view), grouped by name, sorted by CPU then memory.
pub fn processes() -> Vec<Proc> {
    let mut ids = vec![0u32; 4096];
    let mut needed = 0u32;
    loop {
        if unsafe { EnumProcesses(ids.as_mut_ptr(), (ids.len() * 4) as u32, &mut needed) }.is_err() {
            return Vec::new();
        }
        if (needed as usize) < ids.len() * 4 {
            break;
        }
        ids.resize(ids.len() * 2, 0);
    }
    ids.truncate(needed as usize / 4);

    let now = Instant::now();
    let mut prev_guard = PROC_PREV.lock().unwrap_or_else(|e| e.into_inner());
    let prev = prev_guard.take();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    let wall = prev.as_ref().map(|p| now.duration_since(p.at).as_secs_f64() * 1e7 * cores).unwrap_or(0.0);
    let windowed = windowed_pids();
    let me = unsafe { GetCurrentProcessId() };

    let mut cpu_now = HashMap::new();
    let mut groups: HashMap<String, Proc> = HashMap::new();
    for pid in ids {
        if pid == 0 || pid == 4 || pid == me {
            continue;
        }
        let Ok(h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else { continue };
        let h = Handle(h);
        let Some(path) = image_path(h.0) else { continue };
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        if unsafe { GetProcessTimes(h.0, &mut created, &mut exited, &mut kernel, &mut user) }.is_err() {
            continue;
        }
        let total = ft(kernel) + ft(user);
        let key = (pid, ft(created));
        cpu_now.insert(key, total);
        let share = match prev.as_ref().and_then(|p| p.cpu.get(&key)) {
            Some(before) if wall > 0.0 => (total.saturating_sub(*before) as f64 / wall * 100.0) as f32,
            _ => 0.0,
        };

        let file = path.rsplit('\\').next().unwrap_or(&path);
        let name = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file).to_string();
        let g = groups.entry(name.to_lowercase()).or_insert_with(|| Proc { pid, name, path: path.clone(), ..Default::default() });
        g.cpu += share;
        g.mem += private_memory(h.0);
        g.has_window |= windowed.contains(&pid);
        g.pids.push(pid);
    }
    *prev_guard = Some(ProcPrev { cpu: cpu_now, at: now });
    drop(prev_guard);

    let mut list: Vec<Proc> = groups.into_values().collect();
    for p in &mut list {
        p.title = description(&p.path).unwrap_or_else(|| p.name.clone());
    }
    list.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
    list
}

fn image_path(h: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Private working set (what Task Manager's "Memory" column shows); plain working set on older Windows.
fn private_memory(h: HANDLE) -> u64 {
    let mut c = PROCESS_MEMORY_COUNTERS_EX2::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    c.cb = size;
    if unsafe { GetProcessMemoryInfo(h, &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, size) }.is_ok() {
        return if c.PrivateWorkingSetSize > 0 { c.PrivateWorkingSetSize as u64 } else { c.WorkingSetSize as u64 };
    }
    let mut c = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    if unsafe { GetProcessMemoryInfo(h, &mut c, c.cb) }.is_ok() {
        c.WorkingSetSize as u64
    } else {
        0
    }
}

/// Processes owning a visible, unowned, non-tool, non-cloaked top-level window with a title.
fn windowed_pids() -> HashSet<u32> {
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let set = &mut *(lparam.0 as *mut HashSet<u32>);
        if IsWindowVisible(hwnd).as_bool()
            && GetWindow(hwnd, GW_OWNER).map_or(true, |o| o.0.is_null())
            && GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 == 0
            && GetWindowTextLengthW(hwnd) > 0
        {
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
            if cloaked == 0 {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                set.insert(pid);
            }
        }
        true.into()
    }
    let mut set = HashSet::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut set as *mut _ as isize));
    }
    set
}

/// The exe's FileDescription, cached per path.
fn description(path: &str) -> Option<String> {
    let mut cache = DESCRIPTIONS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(d) = cache.get(path) {
        return (!d.is_empty()).then(|| d.clone());
    }
    let d = read_description(path).unwrap_or_default();
    cache.insert(path.to_string(), d.clone());
    (!d.is_empty()).then_some(d)
}

fn read_description(path: &str) -> Option<String> {
    unsafe {
        let file = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&file, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&file, 0, size, data.as_mut_ptr() as _).ok()?;
        let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let mut langs: Vec<(u16, u16)> = Vec::new();
        if VerQueryValueW(data.as_ptr() as _, &HSTRING::from("\\VarFileInfo\\Translation"), &mut p, &mut len).as_bool() && !p.is_null() {
            let pairs = std::slice::from_raw_parts(p as *const u16, len as usize / 2);
            langs.extend(pairs.chunks_exact(2).map(|c| (c[0], c[1])));
        }
        langs.extend([(0x0409, 0x04B0), (0x0409, 0x04E4)]);
        for (lang, cp) in langs {
            let sub = HSTRING::from(format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"));
            if VerQueryValueW(data.as_ptr() as _, PCWSTR(sub.as_ptr()), &mut p, &mut len).as_bool() && !p.is_null() && len > 1 {
                let s = std::slice::from_raw_parts(p as *const u16, len as usize);
                let text = String::from_utf16_lossy(s).trim_end_matches('\0').trim().to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        None
    }
}

/// Ends one process. False when it's gone already or needs administrator rights.
pub fn end(pid: u32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_TERMINATE, false, pid) {
            Ok(h) => {
                let h = Handle(h);
                TerminateProcess(h.0, 1).is_ok()
            }
            Err(_) => false,
        }
    }
}

/// Ends every process of a group (the "End task" button); returns how many could not be ended.
pub fn end_all(pids: &[u32]) -> usize {
    pids.iter().filter(|&&p| !end(p)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gb(b: u64) -> f64 {
        b as f64 / (1u64 << 30) as f64
    }

    #[test]
    #[ignore]
    fn stats_and_processes() {
        let a = sample();
        println!("first:  {a:?}");
        let _ = processes();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let b = sample();
        println!(
            "second: cpu {:.1}%  ram {:.1}/{:.1} GB  down {}  up {}  battery {:?} ({:?}s)  disk {:?}",
            b.cpu,
            gb(b.ram_used),
            gb(b.ram_total),
            rate(b.net_down),
            rate(b.net_up),
            b.battery,
            b.battery_secs,
            b.disk.map(|(u, t)| (gb(u), gb(t)))
        );
        assert!(b.ram_total > 0 && b.ram_used > 0);
        let t = Instant::now();
        let list = processes();
        println!("{} programs in {:?}", list.len(), t.elapsed());
        for p in list.iter().take(12) {
            println!(
                "  {:>6} {:<28} {:<20} {:>5.1}% {:>7.0} MB  x{}  window={}",
                p.pid,
                p.title.chars().take(28).collect::<String>(),
                p.name,
                p.cpu,
                p.mem as f64 / 1048576.0,
                p.pids.len(),
                p.has_window
            );
        }
        println!("with windows: {:?}", list.iter().filter(|p| p.has_window).map(|p| p.name.as_str()).collect::<Vec<_>>());
        assert!(!list.is_empty());
    }
}
