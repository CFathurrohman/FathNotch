//! Opening things with the Windows shell (settings pages, files, folders, URLs).

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Opens a URI, file or folder with its default handler (e.g. "ms-settings:bluetooth").
pub fn open(target: &str) {
    let t = HSTRING::from(target);
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &t, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn open_settings_page(uri: &str) {
    open(uri);
}

/// Opens the app a notification came from: by its AppUserModelId, else by its Start menu name.
/// Runs the shell lookup, so call it off the window thread.
pub fn open_app(app_id: &str, name: &str) {
    let parsing = if app_id.is_empty() { crate::services::appicons::find_app_by_name(name) } else { Some(app_id.to_string()) };
    if let Some(p) = parsing {
        let args = HSTRING::from(format!("shell:AppsFolder\\{p}"));
        unsafe {
            ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from("explorer.exe"), &args, PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}

/// Shows a file selected in Explorer.
pub fn reveal(path: &str) {
    let args = HSTRING::from(format!("/select,\"{path}\""));
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from("explorer.exe"), &args, PCWSTR::null(), SW_SHOWNORMAL);
    }
}
