//! App icons for the Control mixer, System → Apps and Notifications: the shell's icon for an exe, a running
//! process or an installed app (Start menu "AppsFolder" entry by AppUserModelId or display name), as PNG bytes
//! for `Gfx::load_image`.
//!
//! `for_exe` / `for_pid` / `for_app` are blocking. Views use the non-blocking `exe` / `pid` / `app`, which
//! return the icon once it's ready and otherwise queue it on one background worker (results, including
//! "no icon", are kept for the whole run).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// Image key for `Gfx::load_image` / `Gfx::image`, and PNG bytes.
pub type Icon = (u64, Arc<Vec<u8>>);

fn key_of(id: &str) -> u64 {
    let mut h = DefaultHasher::new();
    "appicon".hash(&mut h);
    id.hash(&mut h);
    h.finish()
}

fn icon(id: &str, png: Vec<u8>) -> Icon {
    (key_of(id), Arc::new(png))
}

// ---------------------------------------------------------------- blocking extraction

/// The exe's icon at `px` × `px` pixels.
pub fn for_exe(path: &str, px: u32) -> Option<Icon> {
    if path.is_empty() {
        return None;
    }
    shell_icon(path, px).map(|png| icon(&format!("exe:{px}:{}", path.to_lowercase()), png))
}

/// The icon of a running process's exe.
pub fn for_pid(pid: u32, px: u32) -> Option<Icon> {
    for_exe(&exe_path(pid)?, px)
}

/// A notification sender's icon: the logo the toast came with, else the Start menu entry for its
/// AppUserModelId, else the Start menu entry whose name matches the app's display name.
pub fn for_app(app_id: &str, name: &str, px: u32) -> Option<Icon> {
    let id = format!("app:{px}:{}|{}", app_id.to_lowercase(), name.to_lowercase());
    if let Some(png) = logos().lock().unwrap_or_else(|e| e.into_inner()).get(&logo_key(app_id, name)).cloned() {
        return Some((key_of(&id), png));
    }
    if !app_id.is_empty() {
        if let Some(png) = shell_icon(&format!("shell:AppsFolder\\{app_id}"), px) {
            return Some(icon(&id, png));
        }
    }
    let parsing = find_app_by_name(name)?;
    shell_icon(&format!("shell:AppsFolder\\{parsing}"), px).map(|png| icon(&id, png))
}

/// `for_app` without an AppUserModelId.
pub fn for_app_name(name: &str, px: u32) -> Option<Icon> {
    for_app("", name, px)
}

fn logo_key(app_id: &str, name: &str) -> String {
    if app_id.is_empty() { name.to_lowercase() } else { app_id.to_lowercase() }
}

fn logos() -> &'static Mutex<HashMap<String, Arc<Vec<u8>>>> {
    static L: OnceLock<Mutex<HashMap<String, Arc<Vec<u8>>>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Keeps the logo image a notification arrived with (PNG/JPEG bytes) for `for_app`.
pub fn remember_logo(app_id: &str, name: &str, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    let k = logo_key(app_id, name);
    let fresh = logos().lock().unwrap_or_else(|e| e.into_inner()).insert(k, Arc::new(bytes)).is_none();
    if fresh {
        // Forget earlier "app:" results for it so the logo is used from now on.
        let (id, nm) = (app_id.to_lowercase(), name.to_lowercase());
        slots().lock().unwrap_or_else(|e| e.into_inner()).retain(|s, _| {
            !(s.starts_with("app:") && s.ends_with(&format!("{id}|{nm}")))
        });
    }
}

/// Full path of a process's exe.
pub fn exe_path(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The shell's icon (not a thumbnail) for a parsing name (file path or `shell:AppsFolder\<AUMID>`), as PNG.
fn shell_icon(parsing: &str, px: u32) -> Option<Vec<u8>> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY};
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(parsing), None).ok()?;
        let hbmp = factory.GetImage(SIZE { cx: px as i32, cy: px as i32 }, SIIGBF_ICONONLY).ok()?;
        let mut bm = BITMAP::default();
        GetObjectW(hbmp, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        if w <= 0 || h <= 0 {
            let _ = DeleteObject(hbmp);
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbmp, 0, h as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        let _ = DeleteObject(hbmp);
        if lines == 0 {
            return None;
        }
        // Old-style icons without an alpha channel come back with alpha 0 everywhere.
        if pixels.chunks_exact(4).all(|p| p[3] == 0) {
            for p in pixels.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
        super::shelf::encode_png(w as u32, h as u32, &pixels)
    }
}

/// Start menu apps (shell:AppsFolder) as (lower-case display name, parsing name), read once.
fn apps_folder() -> &'static Vec<(String, String)> {
    static APPS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    APPS.get_or_init(|| unsafe { read_apps_folder() }.unwrap_or_default())
}

unsafe fn read_apps_folder() -> Option<Vec<(String, String)>> {
    use windows::core::w;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY,
        SIGDN_PARENTRELATIVEPARSING,
    };
    let folder: IShellItem = SHCreateItemFromParsingName(w!("shell:AppsFolder"), None).ok()?;
    let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems).ok()?;
    let name_of = |it: &IShellItem, how| -> Option<String> {
        let p = it.GetDisplayName(how).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    };
    let mut out = Vec::new();
    loop {
        let mut one = [None];
        let mut got = 0u32;
        if items.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
            break;
        }
        let Some(it) = one[0].take() else { break };
        if let (Some(name), Some(parsing)) = (name_of(&it, SIGDN_NORMALDISPLAY), name_of(&it, SIGDN_PARENTRELATIVEPARSING)) {
            out.push((name.to_lowercase(), parsing));
        }
    }
    Some(out)
}

/// The AppsFolder parsing name for a display name: an exact (case-insensitive) match, else the shortest
/// entry that starts with it ("Outlook" → "Outlook (new)").
fn find_app_by_name(name: &str) -> Option<String> {
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    let apps = apps_folder();
    if let Some((_, p)) = apps.iter().find(|(a, _)| *a == n) {
        return Some(p.clone());
    }
    apps.iter()
        .filter(|(a, _)| a.starts_with(&n))
        .min_by_key(|(a, _)| a.len())
        .map(|(_, p)| p.clone())
}

// ---------------------------------------------------------------- non-blocking, for views

enum Slot {
    Pending,
    Done(Option<Icon>),
}

fn slots() -> &'static Mutex<HashMap<String, Slot>> {
    static S: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

type Job = Box<dyn FnOnce() + Send>;

/// One worker thread (STA, as shell icon handlers expect) does all extraction in order.
fn worker() -> &'static Mutex<Sender<Job>> {
    static W: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("appicons".into())
            .spawn(move || {
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(
                        None,
                        windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                    );
                }
                for job in rx {
                    job();
                    crate::bg::refresh();
                }
            })
            .ok();
        Mutex::new(tx)
    })
}

fn lazy(id: String, load: impl FnOnce() -> Option<Icon> + Send + 'static) -> Option<Icon> {
    {
        let mut s = slots().lock().unwrap_or_else(|e| e.into_inner());
        match s.get(&id) {
            Some(Slot::Done(i)) => return i.clone(),
            Some(Slot::Pending) => return None,
            None => {}
        }
        s.insert(id.clone(), Slot::Pending);
    }
    let job: Job = Box::new(move || {
        let r = load();
        slots().lock().unwrap_or_else(|e| e.into_inner()).insert(id, Slot::Done(r));
    });
    let _ = worker().lock().unwrap_or_else(|e| e.into_inner()).send(job);
    None
}

/// `for_exe` without blocking: None until loaded (the window is asked to redraw then).
pub fn exe(path: &str, px: u32) -> Option<Icon> {
    let p = path.to_string();
    lazy(format!("exe:{px}:{}", path.to_lowercase()), move || for_exe(&p, px))
}

/// `for_pid` without blocking.
pub fn pid(pid: u32, px: u32) -> Option<Icon> {
    lazy(format!("pid:{px}:{pid}"), move || for_pid(pid, px))
}

/// `for_app` without blocking.
pub fn app(app_id: &str, name: &str, px: u32) -> Option<Icon> {
    let (a, n) = (app_id.to_string(), name.to_string());
    lazy(format!("app:{px}:{}|{}", app_id.to_lowercase(), name.to_lowercase()), move || for_app(&a, &n, px))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn extract() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        }
        let me = super::for_pid(std::process::id(), 32);
        println!("own pid: {:?}", me.map(|i| i.1.len()));
        println!("explorer: {:?}", super::for_exe("C:\\Windows\\explorer.exe", 32).map(|i| i.1.len()));
        println!("apps: {}", super::apps_folder().len());
        for n in ["WhatsApp", "Outlook", "Slack", "Microsoft Edge"] {
            println!("{n}: {:?} {:?}", super::find_app_by_name(n), super::for_app_name(n, 48).map(|i| i.1.len()));
        }
    }
}
