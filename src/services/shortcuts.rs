//! One-click launchers in shortcuts.json, the .NET build's schema: [{ Name, Target, Arguments }].
//! A target is an app, file, folder, URL or ms-settings: page; the defaults are written on first use.

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::store;

const FILE: &str = "shortcuts.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Shortcut {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub arguments: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn sc(name: &str, target: &str, args: &str) -> Shortcut {
    Shortcut { name: name.into(), target: target.into(), arguments: args.into(), extra: Map::new() }
}

fn defaults() -> Vec<Shortcut> {
    let downloads = std::env::var("USERPROFILE").map(|p| format!("{p}\\Downloads")).unwrap_or_default();
    vec![
        sc("Snip screen", "ms-screenclip:", ""),
        sc("Lock PC", "rundll32.exe", "user32.dll,LockWorkStation"),
        sc("Task Manager", "taskmgr.exe", ""),
        sc("Calculator", "calc.exe", ""),
        sc("Notepad", "notepad.exe", ""),
        sc("Downloads", &downloads, ""),
        sc("Bluetooth", "ms-settings:bluetooth", ""),
        sc("Wi-Fi", "ms-settings:network-wifi", ""),
        sc("Sound", "ms-settings:sound", ""),
        sc("Display", "ms-settings:display", ""),
        sc("Focus", "ms-settings:quiethours", ""),
        sc("Battery", "ms-settings:batterysaver", ""),
    ]
}

fn cell() -> &'static Mutex<Vec<Shortcut>> {
    static CELL: OnceLock<Mutex<Vec<Shortcut>>> = OnceLock::new();
    CELL.get_or_init(|| {
        if store::path(FILE).exists() {
            Mutex::new(store::load(FILE))
        } else {
            let d = defaults();
            store::save(FILE, &d);
            Mutex::new(d)
        }
    })
}

fn change(f: impl FnOnce(&mut Vec<Shortcut>)) {
    let mut items = cell().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut items);
    store::save(FILE, &*items);
}

pub fn all() -> Vec<Shortcut> {
    cell().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn add(name: &str, target: &str, args: &str) {
    change(|v| v.push(sc(name, target, args)));
}

pub fn update(index: usize, name: &str, target: &str, args: &str) {
    change(|v| {
        if let Some(s) = v.get_mut(index) {
            s.name = name.into();
            s.target = target.into();
            s.arguments = args.into();
        }
    });
}

pub fn remove(index: usize) {
    change(|v| {
        if index < v.len() {
            v.remove(index);
        }
    });
}

/// Launches a shortcut with the shell. Err carries a message to show.
pub fn run(s: &Shortcut) -> Result<(), String> {
    let r = unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &HSTRING::from(s.target.as_str()),
            &HSTRING::from(s.arguments.as_str()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = r.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(match code {
            2 | 3 => "The system cannot find the file specified".into(),
            5 => "Access is denied".into(),
            31 => "No app is associated with this target".into(),
            _ => format!("Could not run (error {code})"),
        })
    }
}

/// Splits `"C:\path\app.exe" --flag` or `app.exe --flag` into (target, arguments).
pub fn split_command(text: &str) -> (String, String) {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
    }
    let p = std::path::Path::new(text);
    if p.exists() || text.contains("://") || text.ends_with(':') {
        return (text.to_string(), String::new());
    }
    match text.find(' ') {
        Some(i) if i > 0 => (text[..i].to_string(), text[i + 1..].trim().to_string()),
        _ => (text.to_string(), String::new()),
    }
}

/// The editor shows a shortcut as one command line.
pub fn join_command(s: &Shortcut) -> String {
    if s.arguments.is_empty() {
        s.target.clone()
    } else {
        format!("\"{}\" {}", s.target, s.arguments)
    }
}

/// Shows the Open dialog (own STA thread) and returns the picked file(s). Blocking.
pub fn pick_files(title: &str, multi: bool) -> Vec<String> {
    let title = title.to_string();
    std::thread::spawn(move || unsafe {
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{
            FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
        };
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let mut out = Vec::new();
        if let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
            let _ = dlg.SetTitle(&HSTRING::from(title));
            if let Ok(opts) = dlg.GetOptions() {
                let mut o = opts | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST;
                if multi {
                    o |= FOS_ALLOWMULTISELECT;
                }
                let _ = dlg.SetOptions(o);
            }
            if dlg.Show(None).is_ok() {
                if let Ok(items) = dlg.GetResults() {
                    let n = items.GetCount().unwrap_or(0);
                    for i in 0..n {
                        if let Ok(item) = items.GetItemAt(i) {
                            if let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                                out.push(p.to_string().unwrap_or_default());
                                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
                            }
                        }
                    }
                }
            }
        }
        CoUninitialize();
        out
    })
    .join()
    .unwrap_or_default()
}
