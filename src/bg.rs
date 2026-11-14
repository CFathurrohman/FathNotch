//! Background work. Services expose plain blocking functions; the UI runs them here so the window
//! thread never waits on the network or a device. When a job finishes it asks the window to redraw.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the window when background state changed and the notch should redraw.
pub const WM_REFRESH: u32 = WM_APP + 2;

static WINDOW: AtomicIsize = AtomicIsize::new(0);

pub fn init(hwnd: HWND) {
    WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
    // Keep the multithreaded apartment alive for the whole run: windows-rs caches WinRT factories
    // process-wide, and they'd dangle (crash) if the MTA were torn down between background jobs.
    unsafe {
        let _ = windows::Win32::System::Com::CoIncrementMTAUsage();
    }
}

/// Asks the window to redraw (safe from any thread).
pub fn refresh() {
    let h = WINDOW.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as _), WM_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

/// Posted when settings changed (hotkeys, timers and placement are re-applied).
pub const WM_SETTINGS: u32 = WM_APP + 3;

/// Posts `msg` to the window (safe from any thread).
pub fn post(msg: u32) {
    let h = WINDOW.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as _), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// Runs `job` on its own COM-initialised (MTA) thread, then asks for a redraw.
pub fn spawn<F: FnOnce() + Send + 'static>(job: F) {
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        job();
        refresh();
        unsafe { CoUninitialize() };
    });
}
