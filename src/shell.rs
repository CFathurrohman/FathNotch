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

/// Shows a file selected in Explorer.
pub fn reveal(path: &str) {
    let args = HSTRING::from(format!("/select,\"{path}\""));
    unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from("explorer.exe"), &args, PCWSTR::null(), SW_SHOWNORMAL);
    }
}
