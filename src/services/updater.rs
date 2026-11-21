//! Automatic updates from GitHub Releases. A minute after start, and every six hours, the latest release
//! is compared with this build; when it's newer its FathNotch.exe is downloaded, checked (size, "MZ"),
//! and swapped in next to the running exe (Windows lets a running exe be renamed, so the old one becomes
//! FathNotch.old.exe and is deleted on the next start). A pop-up then offers to restart now; otherwise the
//! new version simply starts next time. Dev builds (under target\) and screenshot runs never update.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::http;
use crate::services::settings;

const LATEST: &str = "https://api.github.com/repos/CFathurrohman/FathNotch/releases/latest";
const ASSET: &str = "FathNotch.exe";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What the last check found, for Settings ("Up to date", "Downloading v0.3.0…", "v0.3.0 ready").
static STATUS: Mutex<String> = Mutex::new(String::new());
/// The version already swapped in (waiting for a restart), so it isn't downloaded twice.
static READY: Mutex<Option<String>> = Mutex::new(None);
static BUSY: Mutex<bool> = Mutex::new(false);

pub fn status() -> String {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_status(s: impl Into<String>) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = s.into();
    settings::update(|_| {}); // redraws Settings
}

/// The running exe, unless this is a build that must not replace itself.
fn installed_exe() -> Option<PathBuf> {
    if std::env::var_os("FATHNOTCH_SHOT").is_some() || std::env::var_os("FATHNOTCH_DEMO").is_some() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    (!exe.to_string_lossy().to_lowercase().contains("\\target\\")).then_some(exe)
}

/// Starts the background checks (once, from startup) and removes the exe an earlier update replaced.
pub fn start() {
    let Some(exe) = installed_exe() else { return };
    let _ = std::fs::remove_file(exe.with_file_name("FathNotch.old.exe"));
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            if settings::shared().auto_update {
                check(false);
            }
            std::thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

/// Settings → General → "Check now".
pub fn check_now() {
    crate::bg::spawn(|| check(true));
}

/// "0.2.10" > "0.2.9": numeric parts, left to right.
fn newer(tag: &str, than: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.trim_start_matches(['v', 'V']).split(['.', '-']).map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parts(tag), parts(than));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn check(manual: bool) {
    {
        let mut busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        if *busy {
            return;
        }
        *busy = true;
    }
    let result = run(manual);
    *BUSY.lock().unwrap_or_else(|e| e.into_inner()) = false;
    if let Err(e) = result {
        crate::log(&format!("update check: {e}"));
        if manual {
            set_status(format!("Couldn't check: {e}"));
        }
    }
}

fn run(manual: bool) -> Result<(), String> {
    let Some(exe) = installed_exe() else {
        if manual {
            set_status("Updates are off for development builds");
        }
        return Ok(());
    };
    if manual {
        set_status("Checking…");
    }
    let r = http::send("GET", LATEST, &[("Accept", "application/vnd.github+json")], None).map_err(|e| e.message().to_string())?;
    if !r.ok() {
        return Err(format!("GitHub answered {}", r.status));
    }
    let v: Value = serde_json::from_str(&r.body).map_err(|_| "unexpected answer".to_string())?;
    let tag = v.get("tag_name").and_then(Value::as_str).unwrap_or("").to_string();
    if v.get("draft").and_then(Value::as_bool) == Some(true) || v.get("prerelease").and_then(Value::as_bool) == Some(true) || !newer(&tag, VERSION) {
        set_status(format!("Up to date (v{VERSION})"));
        return Ok(());
    }
    if READY.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(tag.as_str()) {
        set_status(format!("{tag} is ready: restart FathNotch to use it"));
        return Ok(());
    }
    let asset = v
        .get("assets")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|x| x.get("name").and_then(Value::as_str) == Some(ASSET)))
        .ok_or_else(|| format!("{tag} has no {ASSET}"))?;
    let url = asset.get("browser_download_url").and_then(Value::as_str).ok_or("no download link")?;
    let size = asset.get("size").and_then(Value::as_u64).unwrap_or(0);

    set_status(format!("Downloading {tag}…"));
    let bytes = http::get_bytes(url).map_err(|e| format!("download failed: {}", e.message()))?;
    if bytes.len() as u64 != size || size < 100_000 || !bytes.starts_with(b"MZ") {
        return Err("the download looks incomplete".into());
    }
    install(&exe, &bytes)?;
    *READY.lock().unwrap_or_else(|e| e.into_inner()) = Some(tag.clone());
    set_status(format!("{tag} is ready: restart FathNotch to use it"));
    crate::model::push_alert(crate::notch::Alert {
        key: "update".into(),
        kind: crate::notch::AlertKind::Full {
            icon: "ArrowDown",
            title: format!("FathNotch {tag} is ready"),
            sub: "Click to restart and update".into(),
            open: Some("update"),
            app: None,
        },
        duration: Duration::from_secs(15),
    });
    Ok(())
}

/// Puts the new exe in place of the running one: write it next to it, rename the running exe out of the
/// way, rename the new one in. Any failure puts the old one back.
fn install(exe: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    let new = exe.with_file_name("FathNotch.new.exe");
    let old = exe.with_file_name("FathNotch.old.exe");
    std::fs::write(&new, bytes).map_err(|e| format!("can't save the update: {e}"))?;
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(format!("can't replace the app: {e}"));
    }
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&new);
        return Err(format!("can't replace the app: {e}"));
    }
    Ok(())
}

/// Starts the (updated) exe, which waits for this one to quit before taking over; the caller then quits.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        // current_exe still names the original path, where the new version now is.
        let _ = std::process::Command::new(exe).arg("--after-update").spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions() {
        assert!(newer("v0.2.10", "0.2.9"));
        assert!(newer("v0.3.0", "0.2.9"));
        assert!(!newer("v0.2.9", "0.2.9"));
        assert!(!newer("v0.2.8", "0.2.9"));
        assert!(newer("1.0", "0.9.9"));
    }

    /// Downloads the latest release's exe and swaps it into a scratch folder (never the real install).
    #[test]
    #[ignore]
    fn download_and_swap() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let r = crate::http::send("GET", super::LATEST, &[("Accept", "application/vnd.github+json")], None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&r.body).unwrap();
        let tag = v["tag_name"].as_str().unwrap().to_string();
        let asset = v["assets"].as_array().unwrap().iter().find(|a| a["name"] == super::ASSET).unwrap().clone();
        let bytes = crate::http::get_bytes(asset["browser_download_url"].as_str().unwrap()).unwrap();
        println!("{tag}: {} bytes (asset says {}), MZ={}", bytes.len(), asset["size"], bytes.starts_with(b"MZ"));
        assert_eq!(bytes.len() as u64, asset["size"].as_u64().unwrap());
        let dir = std::env::temp_dir().join("fathnotch-update-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("FathNotch.exe");
        std::fs::write(&exe, b"old").unwrap();
        super::install(&exe, &bytes).unwrap();
        assert_eq!(std::fs::metadata(&exe).unwrap().len(), bytes.len() as u64);
        assert_eq!(std::fs::read(dir.join("FathNotch.old.exe")).unwrap(), b"old");
        assert!(!dir.join("FathNotch.new.exe").exists());
        let _ = std::fs::remove_dir_all(&dir);
        println!("swap ok");
    }
}
