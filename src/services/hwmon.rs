//! Live hardware readings for the Hardware tab and the idle notch: CPU (total, per thread, clock), memory,
//! GPUs (use, video memory, temperature on NVIDIA), fixed drives, network rates, ping and battery, plus a
//! minute of history for the graphs. A background thread samples once a second while someone asked for
//! the readings in the last few seconds (`snapshot` asks), and sleeps otherwise, so it costs nothing
//! while neither the tab nor a live idle item is showing.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE};
use windows::Win32::NetworkManagement::IpHelper::{IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_MORE_DATA,
};
use windows::Win32::System::Power::{CallNtPowerInformation, ProcessorInformation};

use crate::services::system;

pub const HISTORY: usize = 60;

#[derive(Clone, Debug, Default)]
pub struct Gpu {
    pub name: String,
    /// 0–100: the busiest engine kind (3D, copy, video…), like Task Manager.
    pub usage: f32,
    pub vram_used: u64,
    pub vram_total: u64,
    pub temp: Option<f32>,
    pub history: VecDeque<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct Disk {
    pub letter: char,
    pub used: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Hw {
    /// Samples so far (the first has no CPU / network rates yet).
    pub samples: u32,
    pub cpu: f32,
    /// Each logical processor, 0–100.
    pub threads: Vec<f32>,
    pub cpu_name: String,
    pub cores: usize,
    pub logical: usize,
    /// Average current clock over all processors, MHz.
    pub mhz: u32,
    pub ram_used: u64,
    pub ram_total: u64,
    pub gpus: Vec<Gpu>,
    pub disks: Vec<Disk>,
    pub net_down: u64,
    pub net_up: u64,
    /// Round trip to 1.1.1.1, ms (None while unknown or unreachable).
    pub ping: Option<u32>,
    pub battery: Option<(u8, bool)>,
    pub battery_secs: Option<u32>,
    pub cpu_history: VecDeque<f32>,
    pub ram_history: VecDeque<f32>,
    /// (down, up) bytes per second.
    pub net_history: VecDeque<(u64, u64)>,
}

struct Shared {
    hw: Hw,
    wanted: Option<Instant>,
}

fn shared() -> &'static Mutex<Shared> {
    static S: OnceLock<Mutex<Shared>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Shared { hw: Hw::default(), wanted: None }))
}

/// The latest readings; also keeps the sampler running for a few more seconds.
pub fn snapshot() -> Hw {
    start();
    let mut s = shared().lock().unwrap_or_else(|e| e.into_inner());
    s.wanted = Some(Instant::now());
    s.hw.clone()
}

/// Just the numbers the idle notch needs (no history copies).
pub fn quick() -> (u32, f32, Option<f32>, f32, u64, u64) {
    start();
    let mut s = shared().lock().unwrap_or_else(|e| e.into_inner());
    s.wanted = Some(Instant::now());
    let h = &s.hw;
    let ram = if h.ram_total > 0 { h.ram_used as f32 * 100.0 / h.ram_total as f32 } else { 0.0 };
    (h.samples, h.cpu, h.gpus.first().map(|g| g.usage), ram, h.net_down, h.net_up)
}

fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    crate::bg::spawn(|| {
        let mut sampler = Sampler::new();
        loop {
            let active = shared().lock().unwrap_or_else(|e| e.into_inner()).wanted.is_some_and(|w| w.elapsed() < Duration::from_secs(4));
            if active {
                let fresh = sampler.sample();
                let mut s = shared().lock().unwrap_or_else(|e| e.into_inner());
                let prev = std::mem::take(&mut s.hw);
                s.hw = merge(prev, fresh);
                drop(s);
                crate::bg::refresh();
                std::thread::sleep(Duration::from_millis(1000));
            } else {
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    });
}

/// Carries the histories over and appends the new values.
fn merge(prev: Hw, mut new: Hw) -> Hw {
    fn push<T>(q: &mut VecDeque<T>, v: T) {
        q.push_back(v);
        while q.len() > HISTORY {
            q.pop_front();
        }
    }
    new.samples = prev.samples + 1;
    new.cpu_history = prev.cpu_history;
    new.ram_history = prev.ram_history;
    new.net_history = prev.net_history;
    if new.ping.is_none() && new.samples % 5 != 1 {
        new.ping = prev.ping; // pinged every 5 s
    }
    if new.samples > 1 {
        push(&mut new.cpu_history, new.cpu);
        push(&mut new.net_history, (new.net_down, new.net_up));
    }
    let ram = if new.ram_total > 0 { new.ram_used as f32 * 100.0 / new.ram_total as f32 } else { 0.0 };
    push(&mut new.ram_history, ram);
    for (i, g) in new.gpus.iter_mut().enumerate() {
        g.history = prev.gpus.get(i).map(|p| p.history.clone()).unwrap_or_default();
        push(&mut g.history, g.usage);
    }
    new
}

// ---------------------------------------------------------------- sampling

struct Adapter {
    vendor: u32,
    name: String,
    luid: String,
    vram_total: u64,
}

struct Sampler {
    query: isize,
    threads: isize,
    gpu_engine: isize,
    gpu_mem: isize,
    adapters: Vec<Adapter>,
    cpu_name: String,
    cores: usize,
    logical: usize,
    nvml: Option<Nvml>,
    n: u64,
    ping: Option<u32>,
}

impl Sampler {
    fn new() -> Self {
        let mut s = Sampler {
            query: 0,
            threads: 0,
            gpu_engine: 0,
            gpu_mem: 0,
            adapters: adapters(),
            cpu_name: cpu_name(),
            cores: physical_cores(),
            logical: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
            nvml: Nvml::load(),
            n: 0,
            ping: None,
        };
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut s.query) == 0 {
                let _ = PdhAddEnglishCounterW(s.query, &HSTRING::from(r"\Processor(*)\% Processor Time"), 0, &mut s.threads);
                let _ = PdhAddEnglishCounterW(s.query, &HSTRING::from(r"\GPU Engine(*)\Utilization Percentage"), 0, &mut s.gpu_engine);
                let _ = PdhAddEnglishCounterW(s.query, &HSTRING::from(r"\GPU Adapter Memory(*)\Dedicated Usage"), 0, &mut s.gpu_mem);
                PdhCollectQueryData(s.query);
            }
        }
        s
    }

    fn sample(&mut self) -> Hw {
        self.n += 1;
        let base = system::sample();
        let mut hw = Hw {
            cpu: base.cpu,
            ram_used: base.ram_used,
            ram_total: base.ram_total,
            net_down: base.net_down,
            net_up: base.net_up,
            battery: base.battery,
            battery_secs: base.battery_secs,
            cpu_name: self.cpu_name.clone(),
            cores: self.cores,
            logical: self.logical,
            mhz: clock_mhz(self.logical),
            disks: disks(),
            ..Default::default()
        };
        if self.query != 0 {
            unsafe { PdhCollectQueryData(self.query) };
            // "_Total" last; the rest are "0", "1", … per logical processor.
            let mut per: Vec<(usize, f32)> = counter_array(self.threads)
                .into_iter()
                .filter_map(|(name, v)| name.parse::<usize>().ok().map(|i| (i, v.clamp(0.0, 100.0) as f32)))
                .collect();
            per.sort_by_key(|(i, _)| *i);
            hw.threads = per.into_iter().map(|(_, v)| v).collect();

            // GPU use: per adapter, sum each engine kind over processes, then the busiest kind.
            let mut kinds: HashMap<(String, String), f64> = HashMap::new();
            for (name, v) in counter_array(self.gpu_engine) {
                let (Some(luid), Some(kind)) = (luid_of(&name), name.split("engtype_").nth(1)) else { continue };
                *kinds.entry((luid, kind.to_string())).or_default() += v;
            }
            let mut mem: HashMap<String, f64> = HashMap::new();
            for (name, v) in counter_array(self.gpu_mem) {
                if let Some(luid) = luid_of(&name) {
                    *mem.entry(luid).or_default() += v;
                }
            }
            for a in &self.adapters {
                let usage = kinds.iter().filter(|((l, _), _)| *l == a.luid).map(|(_, v)| *v).fold(0.0, f64::max);
                hw.gpus.push(Gpu {
                    name: a.name.clone(),
                    usage: usage.clamp(0.0, 100.0) as f32,
                    vram_used: mem.get(&a.luid).copied().unwrap_or(0.0) as u64,
                    vram_total: a.vram_total,
                    temp: self.nvml.as_ref().and_then(|n| n.temp(&a.name)),
                    history: VecDeque::new(),
                });
            }
        }
        if self.n % 5 == 1 {
            self.ping = ping();
        }
        hw.ping = self.ping;
        hw
    }
}

/// (instance name, value) for every instance of a wildcard counter.
fn counter_array(counter: isize) -> Vec<(String, f64)> {
    if counter == 0 {
        return Vec::new();
    }
    unsafe {
        let (mut size, mut count) = (0u32, 0u32);
        let r = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
        if r != PDH_MORE_DATA || size == 0 {
            return Vec::new();
        }
        // The buffer holds the items followed by their names; keep it 8-byte aligned.
        let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(items, count as usize)
            .iter()
            .filter(|it| it.FmtValue.CStatus == 0 || it.FmtValue.CStatus == 1)
            .map(|it| (pwstr(it.szName), it.FmtValue.Anonymous.doubleValue))
            .collect()
    }
}

unsafe fn pwstr(p: PWSTR) -> String {
    if p.is_null() {
        String::new()
    } else {
        p.to_string().unwrap_or_default()
    }
}

/// "pid_12_luid_0x00000000_0x0000D1E3_phys_0_eng_0_engtype_3D" → "0x00000000_0x0000d1e3".
fn luid_of(instance: &str) -> Option<String> {
    let rest = instance.split("luid_").nth(1)?;
    let mut parts = rest.split('_');
    let (hi, lo) = (parts.next()?, parts.next()?);
    Some(format!("{hi}_{lo}").to_lowercase())
}

fn adapters() -> Vec<Adapter> {
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        let mut i = 0;
        while let Ok(a) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(d) = a.GetDesc1() else { continue };
            if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue; // Microsoft Basic Render Driver
            }
            let len = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
            let name = String::from_utf16_lossy(&d.Description[..len]).trim().to_string();
            let luid = format!("0x{:08x}_0x{:08x}", d.AdapterLuid.HighPart as u32, d.AdapterLuid.LowPart);
            if out.iter().any(|x: &Adapter| x.luid == luid) {
                continue;
            }
            out.push(Adapter { vendor: d.VendorId, name: clean_gpu(&name), luid, vram_total: d.DedicatedVideoMemory as u64 });
        }
    }
    // The discrete card first: NVIDIA, then the one with the most video memory.
    out.sort_by(|a, b| (a.vendor != 0x10DE).cmp(&(b.vendor != 0x10DE)).then(b.vram_total.cmp(&a.vram_total)));
    out
}

fn cpu_name() -> String {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let mut buf = [0u16; 128];
    let mut len = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"),
            &HSTRING::from("ProcessorNameString"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    }
    .is_ok();
    if !ok {
        return "CPU".into();
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    // "12th Gen Intel(R) Core(TM) i5-12500H" → "12th Gen Intel Core i5-12500H"
    s.replace("(R)", "").replace("(TM)", "").replace(" CPU", "").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn physical_cores() -> usize {
    use windows::Win32::System::SystemInformation::{GetLogicalProcessorInformation, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION};
    unsafe {
        let mut len = 0u32;
        let _ = GetLogicalProcessorInformation(None, &mut len);
        let n = len as usize / std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION>();
        if n == 0 {
            return 0;
        }
        let mut v = vec![SYSTEM_LOGICAL_PROCESSOR_INFORMATION::default(); n];
        if GetLogicalProcessorInformation(Some(v.as_mut_ptr()), &mut len).is_err() {
            return 0;
        }
        v.iter().filter(|x| x.Relationship == RelationProcessorCore).count()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPowerInformation {
    number: u32,
    max_mhz: u32,
    current_mhz: u32,
    mhz_limit: u32,
    max_idle_state: u32,
    current_idle_state: u32,
}

fn clock_mhz(logical: usize) -> u32 {
    if logical == 0 {
        return 0;
    }
    let mut v = vec![ProcessorPowerInformation::default(); logical];
    let bytes = (v.len() * std::mem::size_of::<ProcessorPowerInformation>()) as u32;
    let st = unsafe { CallNtPowerInformation(ProcessorInformation, None, 0, Some(v.as_mut_ptr() as *mut _), bytes) };
    if st.0 != 0 {
        return 0;
    }
    let sum: u64 = v.iter().map(|p| p.current_mhz as u64).sum();
    (sum / v.len() as u64) as u32
}

fn disks() -> Vec<Disk> {
    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = HSTRING::from(format!("{letter}:\\"));
        if unsafe { GetDriveTypeW(&root) } != 3 {
            continue; // fixed drives only
        }
        let (mut total, mut free) = (0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(&root, None, Some(&mut total), Some(&mut free)) }.is_ok() && total > 0 {
            out.push(Disk { letter, used: total - free, total });
        }
    }
    // The system drive first.
    let sys = std::env::var("SystemDrive").ok().and_then(|d| d.chars().next()).unwrap_or('C');
    out.sort_by_key(|d| (d.letter != sys, d.letter));
    out
}

/// One ICMP echo to 1.1.1.1 (Cloudflare's DNS), ms.
fn ping() -> Option<u32> {
    unsafe {
        let h = IcmpCreateFile().ok()?;
        let data = [0u8; 8];
        let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + data.len() + 16];
        let addr = u32::from_ne_bytes([1, 1, 1, 1]);
        let n = IcmpSendEcho(h, addr, data.as_ptr() as *const _, data.len() as u16, None, reply.as_mut_ptr() as *mut _, reply.len() as u32, 1000);
        let _ = IcmpCloseHandle(h);
        if n == 0 {
            return None;
        }
        let r = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
        (r.Status == 0).then_some(r.RoundTripTime)
    }
}

// ---------------------------------------------------------------- NVIDIA temperature (NVML)

type NvmlInit = unsafe extern "C" fn() -> i32;
type NvmlCount = unsafe extern "C" fn(*mut u32) -> i32;
type NvmlHandle = unsafe extern "C" fn(u32, *mut isize) -> i32;
type NvmlName = unsafe extern "C" fn(isize, *mut u8, u32) -> i32;
type NvmlTemp = unsafe extern "C" fn(isize, i32, *mut u32) -> i32;

/// nvml.dll ships with the NVIDIA driver; loaded only when present.
struct Nvml {
    devices: Vec<(String, isize)>,
    temp: NvmlTemp,
}

impl Nvml {
    fn load() -> Option<Nvml> {
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        unsafe {
            let lib = LoadLibraryW(&HSTRING::from("nvml.dll"))
                .or_else(|_| LoadLibraryW(&HSTRING::from(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll")))
                .ok()?;
            let f = |name: &str| GetProcAddress(lib, windows::core::PCSTR(format!("{name}\0").as_ptr()));
            let init: NvmlInit = std::mem::transmute(f("nvmlInit_v2")?);
            let count: NvmlCount = std::mem::transmute(f("nvmlDeviceGetCount_v2")?);
            let handle: NvmlHandle = std::mem::transmute(f("nvmlDeviceGetHandleByIndex_v2")?);
            let name: NvmlName = std::mem::transmute(f("nvmlDeviceGetName")?);
            let temp: NvmlTemp = std::mem::transmute(f("nvmlDeviceGetTemperature")?);
            if init() != 0 {
                return None;
            }
            let mut n = 0u32;
            if count(&mut n) != 0 {
                return None;
            }
            let mut devices = Vec::new();
            for i in 0..n {
                let mut h = 0isize;
                if handle(i, &mut h) != 0 {
                    continue;
                }
                let mut buf = [0u8; 96];
                let _ = name(h, buf.as_mut_ptr(), buf.len() as u32);
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                devices.push((String::from_utf8_lossy(&buf[..len]).to_string(), h));
            }
            Some(Nvml { devices, temp })
        }
    }

    /// Temperature of the NVIDIA card whose name matches the adapter's.
    fn temp(&self, adapter: &str) -> Option<f32> {
        let a = adapter.to_lowercase();
        let (_, h) = self.devices.iter().find(|(n, _)| {
            let n = n.to_lowercase();
            a.contains(&n) || n.contains(&a)
        })?;
        let mut t = 0u32;
        (unsafe { (self.temp)(*h, 0, &mut t) } == 0 && t > 0 && t < 130).then_some(t as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luids() {
        assert_eq!(luid_of("pid_12_luid_0x00000000_0x0000D1E3_phys_0_eng_0_engtype_3D").as_deref(), Some("0x00000000_0x0000d1e3"));
        assert_eq!(luid_of("luid_0x00000000_0x00011A2B_phys_0").as_deref(), Some("0x00000000_0x00011a2b"));
    }

    /// What this PC reports (hardware names and numbers only).
    #[test]
    #[ignore]
    fn hw_report() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut s = Sampler::new();
        s.sample();
        std::thread::sleep(Duration::from_millis(1100));
        let h = s.sample();
        println!("cpu {:.0}% {} ({}C/{}T, {} MHz) threads={:?}", h.cpu, h.cpu_name, h.cores, h.logical, h.mhz, h.threads.iter().map(|v| *v as i32).collect::<Vec<_>>());
        println!("ram {}/{} MB", h.ram_used >> 20, h.ram_total >> 20);
        for g in &h.gpus {
            println!("gpu {} use {:.0}% vram {}/{} MB temp {:?}", g.name, g.usage, g.vram_used >> 20, g.vram_total >> 20, g.temp);
        }
        for d in &h.disks {
            println!("disk {}: {}/{} GB", d.letter, d.used >> 30, d.total >> 30);
        }
        println!("net {} / {} B/s, ping {:?}, battery {:?}", h.net_down, h.net_up, h.ping, h.battery);
    }
}

/// "AMD Radeon(TM) 660M" → "AMD Radeon 660M".
fn clean_gpu(s: &str) -> String {
    s.replace("(TM)", "").replace("(R)", "").replace(['™', '®'], "").split_whitespace().collect::<Vec<_>>().join(" ")
}
