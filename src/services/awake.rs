//! Keep awake (Control → Awake): while on, Windows doesn't sleep or turn the screen off.
//!
//! `SetThreadExecutionState` holds only while the calling thread lives, so call `apply` from the window
//! thread (it lives as long as the app); quitting the app ends it.

use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};

use crate::services::settings;

/// Turns keep-awake on or off now and remembers it.
pub fn set(on: bool) {
    settings::update(|s| s.keep_awake = on);
    apply(on);
}

/// Tells Windows (no saving): on keeps the PC and screen awake, off lets them sleep as usual.
pub fn apply(on: bool) {
    let flags = if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS };
    unsafe {
        SetThreadExecutionState(flags);
    }
}

pub fn on() -> bool {
    settings::shared().keep_awake
}
