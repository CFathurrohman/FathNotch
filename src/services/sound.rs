//! Alert sounds. The names are the ones the settings store (timer/pomodoro: "No Sound", "Soft Chime", "Bell",
//! "Ping"; reminders/prayer: "Calendar", "Chime", "Bell", "No Sound") and map to Windows' own sounds.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_FILENAME, SND_LOOP, SND_NODEFAULT, SND_SYSTEM};

use crate::services::radios;

/// Plays a timer/pomodoro sound by name (asynchronously). "None"/"No Sound"/"" stay quiet.
pub fn play(name: &str) {
    match name.trim() {
        "" | "None" | "No Sound" => {}
        "Calendar" => {
            let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
            let wav = format!(r"{windir}\Media\Windows Notify Calendar.wav");
            if !(std::path::Path::new(&wav).exists() && file(&wav)) {
                alias("SystemAsterisk");
            }
        }
        "Chime" | "Soft Chime" => {
            alias("SystemAsterisk");
        }
        "Bell" => {
            alias("SystemExclamation");
        }
        _ => {
            alias("SystemDefault");
        }
    }
}

/// An alarm going off: Windows' alarm sound (Alarm01.wav) over and over until `stop`, else the system
/// exclamation (once; the caller repeats it).
pub fn alarm() -> bool {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let wav = format!(r"{windir}\Media\Alarm01.wav");
    if std::path::Path::new(&wav).exists() {
        let path = HSTRING::from(wav.as_str());
        let ok = unsafe {
            PlaySoundW(PCWSTR(path.as_ptr()), HMODULE::default(), SND_FILENAME | SND_ASYNC | SND_LOOP | SND_NODEFAULT | SND_SYSTEM).as_bool()
        };
        if ok {
            return true;
        }
    }
    alias("SystemExclamation");
    false
}

/// Stops a looping alarm sound.
pub fn stop() {
    unsafe {
        let _ = PlaySoundW(PCWSTR::null(), HMODULE::default(), SND_ASYNC);
    }
}

/// Reminder and prayer chimes: like `play`, but quiet while Windows notification sounds are off
/// or the Control tab's Silent tile is on.
pub fn chime(name: &str) {
    if !radios::silent() && !crate::services::settings::shared().silent_mode {
        play(name);
    }
}

fn alias(name: &str) -> bool {
    let name = HSTRING::from(name);
    unsafe { PlaySoundW(PCWSTR(name.as_ptr()), HMODULE::default(), SND_ALIAS | SND_ASYNC | SND_NODEFAULT | SND_SYSTEM).as_bool() }
}

fn file(path: &str) -> bool {
    let path = HSTRING::from(path);
    unsafe { PlaySoundW(PCWSTR(path.as_ptr()), HMODULE::default(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT | SND_SYSTEM).as_bool() }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn plays_a_chime() {
        super::play("Calendar");
        std::thread::sleep(std::time::Duration::from_secs(2));
        super::play("Soft Chime");
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
