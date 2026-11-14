//! Display brightness: the laptop's built-in panel through WMI (root\WMI WmiMonitorBrightness*), external
//! monitors over DDC/CI (dxva2; monitors with DDC/CI turned off in their menu can't be read or set).
//! DDC/CI calls can take tens of milliseconds per monitor, so call these off the UI thread.

use std::collections::HashMap;
use std::sync::{Mutex, Once};
use std::time::Duration;

use windows::core::{w, BSTR, PCWSTR, VARIANT};
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{BOOL, HANDLE, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::System::Com::{
    CoCreateInstance, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
    WBEM_FLAG_RETURN_WBEM_COMPLETE, WBEM_INFINITE,
};

use super::audio::ComScope;

#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// "internal" for the built-in panel, "ddc:<\\.\DISPLAYn>:<index>" for an external monitor.
    pub id: String,
    pub name: String,
    pub level: u32,
    pub internal: bool,
}

const INTERNAL_ID: &str = "internal";

/// DDC/CI brightness range (min, max) per display id, learned by `displays()` and used by `set()`.
static RANGES: Mutex<Option<HashMap<String, (u32, u32)>>> = Mutex::new(None);

/// Every display whose brightness can be read: the built-in panel first, then external monitors.
pub fn displays() -> Vec<Display> {
    let _com = ComScope::new();
    let mut list = Vec::new();
    if let Some(level) = internal_level() {
        list.push(Display { id: INTERNAL_ID.into(), name: "Built-in display".into(), level, internal: true });
    }
    let mut ranges = HashMap::new();
    for_each_physical(|m| {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        if unsafe { GetMonitorBrightness(m.handle, &mut min, &mut cur, &mut max) } == 0 || max <= min {
            return false;
        }
        let label = format!("Display {}", m.display_number);
        let name = if m.description.is_empty() || m.description.to_lowercase().starts_with("generic") {
            label
        } else {
            format!("{} ({label})", m.description)
        };
        let level = ((cur.clamp(min, max) - min) as f64 * 100.0 / (max - min) as f64).round() as u32;
        ranges.insert(m.id.clone(), (min, max));
        list.push(Display { id: m.id.clone(), name, level, internal: false });
        false
    });
    RANGES.lock().unwrap().get_or_insert_with(HashMap::new).extend(ranges);
    list
}

/// Sets a display's brightness (0..100) by the id from `displays()`.
pub fn set(id: &str, level: u32) {
    let level = level.min(100);
    let _com = ComScope::new();
    if id == INTERNAL_ID {
        set_internal(level);
        return;
    }
    for_each_physical(|m| {
        if m.id != id {
            return false;
        }
        let known = RANGES.lock().unwrap().as_ref().and_then(|r| r.get(id).copied());
        let range = known.or_else(|| {
            let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
            (unsafe { GetMonitorBrightness(m.handle, &mut min, &mut cur, &mut max) } != 0 && max > min)
                .then_some((min, max))
        });
        if let Some((min, max)) = range {
            let raw = min + ((max - min) as f64 * level as f64 / 100.0).round() as u32;
            unsafe { SetMonitorBrightness(m.handle, raw) };
        }
        true
    });
}

/// Starts (once) a background thread that calls `on_change(level)` whenever the built-in panel's
/// brightness changes (keys, Action Center, power plan...). Does nothing without a built-in panel.
pub fn watch_internal(on_change: fn(u32)) {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("brightness-watch".into())
            .spawn(move || watch_loop(on_change));
    });
}

// ---------------------------------------------------------------- WMI (built-in panel)

fn wmi() -> Option<IWbemServices> {
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
        let services = locator
            .ConnectServer(&BSTR::from("root\\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
            .ok()?;
        let _ = CoSetProxyBlanket(
            &services,
            10, // RPC_C_AUTHN_WINNT
            0,  // RPC_C_AUTHZ_NONE
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        );
        Some(services)
    }
}

fn query(services: &IWbemServices, wql: &str) -> Vec<IWbemClassObject> {
    let mut out = Vec::new();
    let Ok(rows) = (unsafe {
        services.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(wql),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )
    }) else {
        return out;
    };
    loop {
        let mut row = [None];
        let mut returned = 0;
        let _ = unsafe { rows.Next(WBEM_INFINITE, &mut row, &mut returned) };
        match row[0].take() {
            Some(obj) if returned > 0 => out.push(obj),
            _ => break,
        }
    }
    out
}

fn get_i32(obj: &IWbemClassObject, name: PCWSTR) -> Option<i32> {
    let mut value = VARIANT::default();
    unsafe { obj.Get(name, 0, &mut value, None, None) }.ok()?;
    i32::try_from(&value).ok()
}

fn internal_level() -> Option<u32> {
    let services = wmi()?;
    query(&services, "SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active = TRUE")
        .first()
        .and_then(|o| get_i32(o, w!("CurrentBrightness")))
        .map(|l| l.clamp(0, 100) as u32)
}

fn set_internal(level: u32) -> Option<()> {
    let services = wmi()?;
    unsafe {
        let mut class = None;
        services
            .GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), WBEM_FLAG_RETURN_WBEM_COMPLETE, None, Some(&mut class), None)
            .ok()?;
        let mut signature = None;
        class?.GetMethod(w!("WmiSetBrightness"), 0, &mut signature, std::ptr::null_mut()).ok()?;
        let params = signature?.SpawnInstance(0).ok()?;
        // CIM uint32 travels as VT_I4, CIM uint8 as VT_UI1.
        params.Put(w!("Timeout"), 0, &VARIANT::from(1i32), 0).ok()?;
        params.Put(w!("Brightness"), 0, &VARIANT::from(level as u8), 0).ok()?;
        for instance in query(&services, "SELECT * FROM WmiMonitorBrightnessMethods WHERE Active = TRUE") {
            let mut path = VARIANT::default();
            if instance.Get(w!("__PATH"), 0, &mut path, None, None).is_err() {
                continue;
            }
            let Ok(path) = BSTR::try_from(&path) else { continue };
            let _ = services.ExecMethod(
                &path,
                &BSTR::from("WmiSetBrightness"),
                WBEM_FLAG_RETURN_WBEM_COMPLETE,
                None,
                &params,
                None,
                None,
            );
        }
    }
    Some(())
}

fn watch_loop(on_change: fn(u32)) {
    let _com = ComScope::new();
    if internal_level().is_none() {
        return;
    }
    loop {
        if let Some(services) = wmi() {
            let events = unsafe {
                services.ExecNotificationQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from("SELECT * FROM WmiMonitorBrightnessEvent"),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
            };
            if let Ok(events) = events {
                loop {
                    let mut row = [None];
                    let mut returned = 0;
                    let hr = unsafe { events.Next(WBEM_INFINITE, &mut row, &mut returned) };
                    match row[0].take() {
                        Some(event) if returned > 0 => {
                            if let Some(level) = get_i32(&event, w!("Brightness")) {
                                on_change(level.clamp(0, 100) as u32);
                            }
                        }
                        _ if hr.is_err() => break,
                        _ => {}
                    }
                }
            }
        }
        // The subscription broke (WMI service restarted...): try again shortly.
        std::thread::sleep(Duration::from_secs(5));
    }
}

// ---------------------------------------------------------------- DDC/CI (external monitors)

struct Physical {
    id: String,
    description: String,
    handle: HANDLE,
    /// 1-based position in the monitor enumeration, for labels.
    display_number: usize,
}

/// Calls `f` for each physical monitor of every display until it returns true; handles are only valid
/// during the call.
fn for_each_physical(mut f: impl FnMut(&Physical) -> bool) {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        (*(data.0 as *mut Vec<HMONITOR>)).push(monitor);
        TRUE
    }
    let mut monitors: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(collect), LPARAM(&mut monitors as *mut _ as isize));
    }
    for (n, monitor) in monitors.into_iter().enumerate() {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
            continue;
        }
        let device = wide_to_string(&info.szDevice);
        let mut count = 0u32;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count) }.is_err() || count == 0 {
            continue;
        }
        let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(monitor, &mut physical) }.is_err() {
            continue;
        }
        let mut done = false;
        for (i, p) in physical.iter().enumerate() {
            let description = { p.szPhysicalMonitorDescription };
            let m = Physical {
                id: format!("ddc:{device}:{i}"),
                description: wide_to_string(&description).trim().to_string(),
                handle: { p.hPhysicalMonitor },
                display_number: n + 1,
            };
            if f(&m) {
                done = true;
                break;
            }
        }
        let _ = unsafe { DestroyPhysicalMonitors(&physical) };
        if done {
            return;
        }
    }
}

fn wide_to_string(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

// ---------------------------------------------------------------- tests (run manually)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn brightness_print_displays() {
        for d in displays() {
            println!("{:<12} {:<40} level {:>3} internal {}", d.id, d.name, d.level, d.internal);
        }
    }

    #[test]
    #[ignore]
    fn brightness_set_and_restore() {
        for d in displays() {
            let probe = if d.level > 50 { d.level - 5 } else { d.level + 5 };
            set(&d.id, probe);
            std::thread::sleep(Duration::from_millis(300));
            let got = displays().into_iter().find(|x| x.id == d.id).map(|x| x.level);
            set(&d.id, d.level);
            std::thread::sleep(Duration::from_millis(300));
            let back = displays().into_iter().find(|x| x.id == d.id).map(|x| x.level);
            println!("{}: {} -> {:?} -> {:?}", d.id, d.level, got, back);
        }
    }

    #[test]
    #[ignore]
    fn brightness_watch() {
        fn cb(level: u32) {
            println!("watch: brightness {level}");
        }
        watch_internal(cb);
        std::thread::sleep(Duration::from_millis(1000));
        if let Some(d) = displays().into_iter().find(|d| d.internal) {
            set(&d.id, if d.level > 50 { d.level - 5 } else { d.level + 5 });
            std::thread::sleep(Duration::from_millis(1000));
            set(&d.id, d.level);
            std::thread::sleep(Duration::from_millis(1000));
        }
    }
}
