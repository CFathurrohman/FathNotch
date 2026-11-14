//! Windows notifications from other apps, for the notch's alerts and the Notifications tab.
//!
//! Uses Windows' official `UserNotificationListener`, polled every 2 s on a background thread (its
//! change event needs package identity). It works unpackaged on current Windows 11 (verified); when
//! Windows refuses access, `problem()` says why, `history()` stays empty, and access is re-checked every 30 s.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u32,
    /// The sending app's display name.
    pub app: String,
    /// The sending app's AppUserModelId ("" when unknown); used to find its icon.
    pub app_id: String,
    pub title: String,
    pub body: String,
    pub arrived: SystemTime,
    /// For FathNotch's own alerts (phone, timers, prayer…): the icon drawn in the app tile ("" for apps).
    pub icon: String,
}

const KEEP: usize = 50;
const POLL: Duration = Duration::from_secs(2);
/// After access is refused, check again this often (the user may grant it in Settings).
const RETRY: Duration = Duration::from_secs(30);

struct State {
    history: Vec<Notification>,
    problem: Option<String>,
}

/// The history survives restarts (30 days), in its own file.
const FILE: &str = "notifications-history.json";
const KEEP_DAYS: u64 = 30;

#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    id: u32,
    app: String,
    #[serde(default)]
    app_id: String,
    title: String,
    body: String,
    arrived: u64,
    #[serde(default)]
    icon: String,
}

fn load_saved() -> Vec<Notification> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let saved: Vec<Saved> = crate::services::store::load(FILE);
    saved
        .into_iter()
        .filter(|s| now.saturating_sub(s.arrived) < KEEP_DAYS * 86_400)
        .map(|s| Notification { id: s.id, app: s.app, app_id: s.app_id, title: s.title, body: s.body, arrived: UNIX_EPOCH + Duration::from_secs(s.arrived), icon: s.icon })
        .collect()
}

fn save(history: &[Notification]) {
    let saved: Vec<Saved> = history
        .iter()
        .map(|n| Saved {
            id: n.id,
            app: n.app.clone(),
            app_id: n.app_id.clone(),
            title: n.title.clone(),
            body: n.body.clone(),
            arrived: n.arrived.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            icon: n.icon.clone(),
        })
        .collect();
    crate::services::store::save(FILE, &saved);
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { history: load_saved(), problem: None }))
}

/// Removes one notification from the history.
pub fn remove(id: u32) {
    let mut s = lock();
    s.history.retain(|n| n.id != id);
    save(&s.history);
}

/// Ids for FathNotch's own entries live above Windows' (which count up from small numbers).
const LOCAL_IDS: u32 = 0x8000_0000;

/// Adds one of FathNotch's own alerts (phone transfer, timer, prayer, reminder…) to the history.
pub fn add_local(source: &str, icon: &str, title: &str, body: &str) {
    let mut s = lock();
    let id = s.history.iter().map(|n| n.id).filter(|&i| i >= LOCAL_IDS).max().map_or(LOCAL_IDS, |i| i.wrapping_add(1).max(LOCAL_IDS));
    let n = Notification {
        id,
        app: source.to_string(),
        app_id: String::new(),
        title: title.to_string(),
        body: body.to_string(),
        arrived: SystemTime::now(),
        icon: icon.to_string(),
    };
    s.history.insert(0, n);
    s.history.truncate(KEEP);
    save(&s.history);
    drop(s);
    crate::bg::refresh();
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts watching (once). `on_new` runs on the watcher thread for each notification that arrives
/// after this call; the UI is also asked to redraw.
pub fn start(on_new: fn(Notification)) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("notifications".into())
        .spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_MULTITHREADED,
                );
            }
            watch(on_new);
        })
        .ok();
}

/// Received notifications, newest first (the last 50).
pub fn history() -> Vec<Notification> {
    lock().history.clone()
}

pub fn clear() {
    let mut s = lock();
    s.history.clear();
    save(&s.history);
}

/// Why notifications can't be read, or None while they can.
pub fn problem() -> Option<String> {
    lock().problem.clone()
}

fn set_problem(p: Option<&str>) {
    let mut s = lock();
    let p = p.map(str::to_string);
    if s.problem != p {
        s.problem = p;
        drop(s);
        crate::bg::refresh();
    }
}

/// Gets the listener and access to it, or the reason not.
fn listener() -> Result<UserNotificationListener, &'static str> {
    let l = UserNotificationListener::Current()
        .map_err(|_| "Windows only lets packaged apps read notifications.")?;
    let mut status = l.GetAccessStatus().unwrap_or(UserNotificationListenerAccessStatus::Unspecified);
    if status == UserNotificationListenerAccessStatus::Unspecified {
        status = l
            .RequestAccessAsync()
            .and_then(|op| op.get())
            .map_err(|_| "Windows only lets packaged apps read notifications.")?;
    }
    match status {
        UserNotificationListenerAccessStatus::Allowed => Ok(l),
        UserNotificationListenerAccessStatus::Denied => {
            Err("Notification access is off for FathNotch in Settings → Privacy & security → Notifications.")
        }
        _ => Err("Windows hasn't granted notification access."),
    }
}

fn watch(on_new: fn(Notification)) {
    let mut seen: Option<HashSet<u32>> = None; // None until the first successful read (the baseline)
    loop {
        let l = match listener() {
            Ok(l) => l,
            Err(why) => {
                set_problem(Some(why));
                std::thread::sleep(RETRY);
                continue;
            }
        };
        set_problem(None);
        loop {
            let Ok(list) = l.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get()) else {
                break; // access revoked or the service restarted: re-acquire
            };
            let mut now_ids = HashSet::new();
            let mut fresh = Vec::new();
            for n in list {
                let Ok(id) = n.Id() else { continue };
                now_ids.insert(id);
                if seen.as_ref().is_some_and(|s| !s.contains(&id)) {
                    if let Some(parsed) = read(&n, id) {
                        fresh.push(parsed);
                    }
                }
            }
            seen = Some(now_ids);
            if !fresh.is_empty() {
                fresh.sort_by_key(|n| n.arrived);
                {
                    let mut s = lock();
                    for n in &fresh {
                        s.history.insert(0, n.clone());
                    }
                    s.history.truncate(KEEP);
                    save(&s.history);
                }
                for n in fresh {
                    on_new(n);
                }
                crate::bg::refresh();
            }
            std::thread::sleep(POLL);
        }
        std::thread::sleep(POLL);
    }
}

/// App name and the ToastGeneric texts: the first is the title, the rest the body.
fn read(n: &UserNotification, id: u32) -> Option<Notification> {
    let app = n
        .AppInfo()
        .and_then(|a| a.DisplayInfo())
        .and_then(|d| d.DisplayName())
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    if app.to_lowercase().contains("fathnotch") {
        return None; // our own toasts
    }
    let app_id = n.AppInfo().and_then(|a| a.AppUserModelId()).map(|s| s.to_string_lossy()).unwrap_or_default();
    let binding = n
        .Notification()
        .and_then(|t| t.Visual())
        .and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric()?))
        .ok()?;
    let texts: Vec<String> = binding
        .GetTextElements()
        .ok()?
        .into_iter()
        .filter_map(|t| t.Text().ok().map(|s| s.to_string_lossy().trim().to_string()))
        .filter(|t| !t.is_empty())
        .collect();
    let (title, body) = match texts.split_first() {
        Some((first, rest)) => (first.clone(), rest.join(" ")),
        // Some apps (Edge site notifications) put their text where these elements don't reach.
        None => ("New notification".to_string(), String::new()),
    };
    let arrived = n
        .CreationTime()
        .ok()
        .and_then(|d| {
            const EPOCH_DIFF: i64 = 116_444_736_000_000_000; // 1601 → 1970 in 100 ns ticks
            let since = d.UniversalTime.checked_sub(EPOCH_DIFF)?;
            UNIX_EPOCH.checked_add(Duration::from_nanos(u64::try_from(since).ok()? * 100))
        })
        .unwrap_or_else(SystemTime::now);
    fetch_logo(id, &app_id, &app);
    Some(Notification { id, app, app_id, title, body, arrived, icon: String::new() })
}

/// Loads the app's logo on its own thread, once per app: for some apps (WhatsApp) Windows never finishes
/// opening it, and waiting on the watcher thread froze it so no notification got through.
fn fetch_logo(id: u32, app_id: &str, app: &str) {
    static TRIED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = format!("{app_id}|{app}");
    if !TRIED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(key) {
        return;
    }
    let (app_id, app) = (app_id.to_string(), app.to_string());
    std::thread::Builder::new()
        .name("notification-logo".into())
        .spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            let n = UserNotificationListener::Current().and_then(|l| l.GetNotification(id));
            if let Some(bytes) = n.ok().and_then(|n| logo(&n)) {
                crate::services::appicons::remember_logo(&app_id, &app, bytes);
                crate::bg::refresh();
            }
        })
        .ok();
}

/// The app logo Windows shows with the toast (64 px), as image file bytes.
fn logo(n: &UserNotification) -> Option<Vec<u8>> {
    use windows::Foundation::Size;
    use windows::Storage::Streams::DataReader;
    let stream = n
        .AppInfo()
        .and_then(|a| a.DisplayInfo())
        .and_then(|d| d.GetLogo(Size { Width: 64.0, Height: 64.0 }))
        .and_then(|r| r.OpenReadAsync())
        .and_then(|op| op.get())
        .ok()?;
    let len = u32::try_from(stream.Size().ok()?).ok()?;
    if len == 0 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    let got = reader.LoadAsync(len).and_then(|op| op.get()).ok()?;
    let mut buf = vec![0u8; got as usize];
    reader.ReadBytes(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn notification_access() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        match listener() {
            Ok(l) => {
                let list = l.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.get());
                match list {
                    Ok(list) => {
                        println!("access allowed, {} toasts in the action center", list.Size().unwrap_or(0));
                        for n in list {
                            let id = n.Id().unwrap_or(0);
                            println!("  {:?}", read(&n, id));
                        }
                    }
                    Err(e) => println!("access allowed but reading failed: {e:?}"),
                }
            }
            Err(why) => println!("no access: {why}"),
        }
        let raw = UserNotificationListener::Current().and_then(|l| l.RequestAccessAsync()?.get());
        println!("raw RequestAccessAsync: {raw:?}");
        start(|n| println!("new: {n:?}"));
        // Time to raise a toast from elsewhere and watch it arrive.
        let wait = std::env::var("FATHNOTCH_NOTIFY_WAIT").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
        std::thread::sleep(Duration::from_secs(wait));
        println!("problem: {:?}, history: {}", problem(), history().len());
    }
}
