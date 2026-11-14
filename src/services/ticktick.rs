//! Two-way sync between the to-dos and one TickTick list, through TickTick's Open API (v1) with a personal
//! API token (TickTick → Settings → Account → API Token), the same way the .NET build does it.
//!
//! Each sync: deletions and completions made here are sent; open to-dos made here are created there; edits
//! (text, priority/star, due date) go whichever way they were made; tasks added in TickTick come in; a linked
//! task that's gone from the list's open tasks (completed or deleted there) is ticked off here.
//!
//! The network work runs without holding the to-do list, so the UI can keep editing; results are applied
//! afterwards and skip items the user changed meanwhile (they go out on the next sync).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::settings;
use super::todos::{self, Todo};

const BASE: &str = "https://api.ticktick.com/open/v1";

#[derive(Clone, Debug, Default)]
pub struct SyncReport {
    /// Open tasks found in the list (None when the list couldn't be read).
    pub remote_open: Option<usize>,
    pub created_there: usize,
    pub updated_there: usize,
    pub completed_there: usize,
    pub deleted_there: usize,
    pub added_here: usize,
    pub updated_here: usize,
    pub closed_here: usize,
}

// ---------------------------------------------------------------- status

struct State {
    status: String,
    failed: bool,
    last_sync: Option<Instant>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { status: String::new(), failed: false, last_sync: None }))
}

fn set_status(text: impl Into<String>, failed: bool) {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    s.status = text.into();
    s.failed = failed;
    if !failed {
        s.last_sync = Some(Instant::now());
    }
}

/// "Synced 10:42", "Syncing…", "Off", or what went wrong.
pub fn status() -> String {
    state().lock().unwrap_or_else(|e| e.into_inner()).status.clone()
}

pub fn failed() -> bool {
    state().lock().unwrap_or_else(|e| e.into_inner()).failed
}

/// On in settings and a token saved.
pub fn enabled() -> bool {
    settings::get().ticktick_enabled && settings::has_ticktick_token()
}

// ---------------------------------------------------------------- HTTP

enum ApiError {
    NoToken,
    Network(String),
}

fn api(method: &str, path: &str, body: Option<&Value>) -> Result<(u16, Option<Value>), ApiError> {
    let token = settings::ticktick_token().ok_or(ApiError::NoToken)?;
    let auth = format!("Bearer {token}");
    let text = body.map(|b| b.to_string());
    let r = crate::http::send(method, &format!("{BASE}{path}"), &[("Authorization", &auth)], text.as_deref())
        .map_err(|e| ApiError::Network(e.message()))?;
    let json = if r.body.trim().is_empty() { None } else { serde_json::from_str(&r.body).ok() };
    Ok((r.status, json))
}

fn api_err(e: ApiError) -> String {
    match e {
        ApiError::NoToken => "No TickTick token (or it can't be decrypted on this account).".into(),
        ApiError::Network(_) => "Offline: will sync when you're back online".into(),
    }
}

/// The account's lists for Settings: (id, name), Inbox ("") first.
pub fn projects() -> Result<Vec<(String, String)>, String> {
    let (code, body) = api("GET", "/project", None).map_err(api_err)?;
    if code == 401 {
        return Err("The API token was refused.".into());
    }
    if !(200..300).contains(&code) {
        return Err(format!("TickTick answered {code}"));
    }
    let mut list = vec![(String::new(), "Inbox".to_string())];
    for p in body.as_ref().and_then(Value::as_array).into_iter().flatten() {
        if p["closed"].as_bool() == Some(true) {
            continue;
        }
        if let Some(id) = p["id"].as_str() {
            list.push((id.to_string(), p["name"].as_str().unwrap_or(id).to_string()));
        }
    }
    Ok(list)
}

/// The list to sync with: the chosen one, else the Inbox (its real id once known).
fn project_id(s: &settings::Settings) -> String {
    if !s.ticktick_project.is_empty() {
        s.ticktick_project.clone()
    } else if !s.ticktick_inbox_id.is_empty() {
        s.ticktick_inbox_id.clone()
    } else {
        "inbox".into()
    }
}

/// Open tasks of a list by id, or None when the list couldn't be read.
fn open_tasks(project: &str) -> Result<(u16, Option<HashMap<String, Value>>), ApiError> {
    let (code, data) = api("GET", &format!("/project/{project}/data"), None)?;
    let tasks = (code == 200)
        .then(|| data.as_ref().and_then(|d| d["tasks"].as_array()))
        .flatten()
        .map(|tasks| {
            tasks
                .iter()
                .filter(|t| t["status"].as_i64().unwrap_or(0) == 0)
                .filter_map(|t| Some((t["id"].as_str()?.to_string(), t.clone())))
                .collect()
        });
    Ok((code, tasks))
}

// ---------------------------------------------------------------- dates

/// The IANA name of this PC's time zone ("Asia/Jakarta"), from Windows' ICU data.
fn iana_zone() -> Option<String> {
    let cal = windows::Globalization::Calendar::new().ok()?;
    let z = cal.GetTimeZone().ok()?.to_string_lossy();
    (!z.is_empty()).then_some(z)
}

/// The to-do's due date in TickTick's form: UTC "yyyy-MM-dd'T'HH:mm:ss+0000", all-day ones at local
/// midnight, plus the time zone so TickTick shows it on the right day. No date sends nulls (clears it).
fn add_due(body: &mut Value, t: &Todo) {
    let local = t.due.as_deref().and_then(todos::parse_local).map(|d| d.0);
    let Some(mut local) = local else {
        body["dueDate"] = Value::Null;
        body["startDate"] = Value::Null;
        return;
    };
    if t.due_all_day {
        local = local.div_euclid(86_400) * 86_400;
    }
    let utc = todos::utc_from_local(local);
    let (y, m, d) = todos::civil_from_days(utc.div_euclid(86_400));
    let s = utc.rem_euclid(86_400);
    let text = format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+0000", s / 3600, s % 3600 / 60, s % 60);
    body["dueDate"] = json!(text);
    body["startDate"] = json!(text);
    body["isAllDay"] = json!(t.due_all_day);
    if let Some(z) = iana_zone() {
        body["timeZone"] = json!(z);
    }
}

/// TickTick's "2019-11-13T03:00:00.000+0000" → the .NET-style local string stored in todos.json.
fn parse_due(text: Option<&str>) -> Option<String> {
    let (local, _) = todos::parse_local(text?.trim())?;
    Some(todos::format_local(local, 0))
}

/// TickTick's title, priority (high = starred) and due date onto a local to-do.
fn apply_remote(t: &mut Todo, task: &Value) {
    if let Some(title) = task["title"].as_str() {
        t.text = title.to_string();
    }
    t.tick_priority = task["priority"].as_i64().unwrap_or(0) as i32;
    t.starred = t.tick_priority >= todos::PRIORITY_HIGH;
    t.due_all_day = task["isAllDay"].as_bool().unwrap_or(false);
    t.due = parse_due(task["dueDate"].as_str());
    t.needs_push = false;
}

/// What `apply_remote` would change, to count real updates.
fn differs(t: &Todo, task: &Value) -> bool {
    let mut copy = t.clone();
    apply_remote(&mut copy, task);
    copy.text != t.text || copy.tick_priority != t.tick_priority || copy.due != t.due || copy.due_all_day != t.due_all_day
}

// ---------------------------------------------------------------- sync

/// One full sync (blocking). Safe to call from any thread; concurrent calls wait for each other.
pub fn sync() -> Result<SyncReport, String> {
    static RUNNING: Mutex<()> = Mutex::new(());
    let _guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if !settings::has_ticktick_token() {
        set_status("", false);
        return Err("No TickTick token".into());
    }
    set_status("Syncing…", false);
    match run_sync() {
        Ok(report) => {
            if report.remote_open.is_some() {
                set_status(format!("Synced {}", clock_now()), false);
            } else {
                set_status("Sent your changes. Pick a list in Settings → To-Dos to also bring TickTick's tasks in.", true);
            }
            Ok(report)
        }
        Err(msg) => {
            set_status(msg.clone(), true);
            Err(msg)
        }
    }
}

fn clock_now() -> String {
    let s = todos::local_now_secs().rem_euclid(86_400);
    format!("{:02}:{:02}", s / 3600, s % 3600 / 60)
}

fn ok_or_gone(code: u16) -> bool {
    matches!(code, 200 | 204 | 404)
}

fn run_sync() -> Result<SyncReport, String> {
    let cfg = settings::get();
    let project = project_id(&cfg);
    let mut report = SyncReport::default();

    // 1. What's open in the list now.
    let (code, remote) = open_tasks(&project).map_err(api_err)?;
    if code == 401 {
        return Err("The API token was refused. Paste a new one in Settings → To-Dos.".into());
    }
    let pulled = remote.is_some();
    let mut remote = remote.unwrap_or_default();
    report.remote_open = pulled.then_some(remote.len());

    let (items, deleted) = todos::snapshot();

    // 2. Deletions and completions made here.
    let mut sent_refs = HashSet::new();
    for gone in &deleted {
        let pid = if gone.project_id.is_empty() { project.as_str() } else { gone.project_id.as_str() };
        let r = if gone.complete {
            api("POST", &format!("/project/{pid}/task/{}/complete", gone.id), None)
        } else {
            api("DELETE", &format!("/project/{pid}/task/{}", gone.id), None)
        };
        let (c, _) = r.map_err(api_err)?;
        if ok_or_gone(c) {
            sent_refs.insert(gone.id.clone());
            if gone.complete { report.completed_there += 1 } else { report.deleted_there += 1 }
        }
        remote.remove(&gone.id);
    }
    if !sent_refs.is_empty() {
        todos::with_store(false, |s| s.deleted.retain(|d| !sent_refs.contains(&d.id)));
    }

    // 3–6. Each local to-do. Results are collected as (id, rev at snapshot, change) and applied after.
    enum Change {
        Created { tick_id: String, project: String, priority: i32 },
        Completed,
        Pushed { priority: i32 },
        Remote(Value),
        ClosedThere,
    }
    let mut changes: Vec<(String, u64, Change)> = vec![];
    let mut learned_inbox: Option<String> = None;

    for t in &items {
        let Some(tid) = t.tick_id.clone() else {
            // 3. New here: create it there (open ones only).
            if t.done {
                continue;
            }
            let priority = if t.starred { todos::PRIORITY_HIGH } else { t.tick_priority };
            let mut body = json!({ "title": t.text, "priority": priority });
            add_due(&mut body, t);
            if !cfg.ticktick_project.is_empty() {
                body["projectId"] = json!(cfg.ticktick_project);
            }
            let (c, created) = api("POST", "/task", Some(&body)).map_err(api_err)?;
            let Some(created) = created.filter(|_| c == 200) else { continue };
            let Some(new_id) = created["id"].as_str() else { continue };
            let pid = created["projectId"].as_str().unwrap_or("").to_string();
            if cfg.ticktick_project.is_empty() && !pid.is_empty() && cfg.ticktick_inbox_id != pid {
                learned_inbox = Some(pid.clone());
            }
            report.created_there += 1;
            changes.push((t.id.clone(), t.rev, Change::Created { tick_id: new_id.into(), project: pid, priority }));
            continue;
        };

        let pid = t.tick_project.clone().filter(|p| !p.is_empty()).unwrap_or_else(|| project.clone());
        if t.done {
            // 4. Ticked off here.
            if t.synced_done {
                continue;
            }
            let (c, _) = api("POST", &format!("/project/{pid}/task/{tid}/complete"), None).map_err(api_err)?;
            if ok_or_gone(c) {
                report.completed_there += 1;
                changes.push((t.id.clone(), t.rev, Change::Completed));
            }
            continue;
        }

        if let Some(task) = remote.remove(&tid) {
            if t.needs_push {
                // 5a. Edited here: send it.
                let priority = if t.starred {
                    todos::PRIORITY_HIGH
                } else if t.tick_priority == todos::PRIORITY_HIGH {
                    0
                } else {
                    t.tick_priority
                };
                let mut body = json!({ "id": tid, "projectId": pid, "title": t.text, "priority": priority });
                add_due(&mut body, t);
                let (c, _) = api("POST", &format!("/task/{tid}"), Some(&body)).map_err(api_err)?;
                if c == 200 {
                    report.updated_there += 1;
                    changes.push((t.id.clone(), t.rev, Change::Pushed { priority }));
                }
            } else if differs(t, &task) {
                // 5b. Otherwise take TickTick's version.
                report.updated_here += 1;
                changes.push((t.id.clone(), t.rev, Change::Remote(task)));
            }
        } else if pulled && pid == project {
            // 6. Open here but not open there: completed (or deleted) in TickTick.
            report.closed_here += 1;
            changes.push((t.id.clone(), t.rev, Change::ClosedThere));
        }
    }

    if let Some(inbox) = learned_inbox {
        settings::update_quiet(|s| s.ticktick_inbox_id = inbox);
    }

    // Apply everything in one go; 7. bring in tasks new in TickTick.
    let added = todos::with_store(false, |s| {
        for (id, rev, change) in changes {
            let Some(t) = s.items.iter_mut().find(|t| t.id == id) else { continue };
            let untouched = t.rev == rev; // the user didn't change it while we were talking to TickTick
            match change {
                Change::Created { tick_id, project, priority } => {
                    t.tick_id = Some(tick_id);
                    t.tick_project = Some(project);
                    t.tick_priority = priority;
                    if untouched {
                        t.needs_push = false;
                    } else {
                        t.needs_push = true; // send the newer edit next time
                    }
                }
                Change::Completed => t.synced_done = true,
                Change::Pushed { priority } => {
                    if untouched {
                        t.needs_push = false;
                        t.tick_priority = priority;
                    }
                }
                Change::Remote(task) => {
                    if untouched && !t.needs_push {
                        apply_remote(t, &task);
                    }
                }
                Change::ClosedThere => {
                    if untouched && !t.done {
                        t.done = true;
                        t.synced_done = true;
                    }
                }
            }
        }

        let known: HashSet<String> = s
            .deleted
            .iter()
            .map(|d| d.id.clone())
            .chain(s.items.iter().filter_map(|t| t.tick_id.clone()))
            .collect();
        let mut added = 0;
        for (tid, task) in &remote {
            if known.contains(tid) {
                continue;
            }
            let mut t = todos::add_detached();
            t.tick_id = Some(tid.clone());
            t.tick_project = Some(task["projectId"].as_str().map(str::to_string).unwrap_or_else(|| project.clone()));
            apply_remote(&mut t, task);
            s.items.push(t);
            added += 1;
        }
        added
    });
    report.added_here = added;
    Ok(report)
}

// ---------------------------------------------------------------- background

static STARTED: AtomicBool = AtomicBool::new(false);
static SOON: AtomicBool = AtomicBool::new(false);

/// Asks the background loop to sync within a couple of seconds (e.g. when the To-Dos tab opens).
pub fn sync_soon() {
    SOON.store(true, Ordering::Relaxed);
}

/// Starts the background loop (once): a full sync every `interval_secs`, a few seconds after local
/// changes, and on `sync_soon`. Each sync asks the window to redraw.
pub fn start_auto(interval_secs: u64) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let interval = Duration::from_secs(interval_secs.max(30));
    std::thread::spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let mut last_try: Option<Instant> = None;
        let mut first = true;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            if !enabled() {
                let text = if settings::has_ticktick_token() { "Off" } else { "" };
                if status() != text {
                    set_status(text, false);
                    crate::bg::refresh();
                }
                last_try = None;
                continue;
            }
            let since = last_try.map(|t| t.elapsed());
            let full_due = since.map_or(true, |e| e >= interval);
            // After a failure, local changes wait a minute instead of retrying every few seconds.
            let backoff = if failed() { Duration::from_secs(60) } else { Duration::from_secs(4) };
            let (pending, last_edit) = todos::pending_sync();
            let settled = last_edit.map_or(true, |e| e.elapsed() >= Duration::from_secs(4));
            let changes_due = pending && settled && since.map_or(true, |e| e >= backoff);
            let asked = SOON.swap(false, Ordering::Relaxed);
            if first || full_due || changes_due || asked {
                first = false;
                last_try = Some(Instant::now());
                let _ = sync();
                crate::bg::refresh();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::settings::tests::scratch;

    /// Read-only: lists projects and the synced list's open tasks, and checks that TickTick's due dates
    /// parse to the same values the .NET build stored for the linked to-dos. Creates/changes nothing.
    #[test]
    #[ignore]
    fn ticktick_read_only() {
        scratch();
        println!("token decrypts: {}", settings::ticktick_token().is_some());
        println!("IANA zone: {:?}", iana_zone());
        let projects = projects().unwrap_or_else(|e| panic!("projects: {e}"));
        println!("{} lists (incl. Inbox)", projects.len());
        let cfg = settings::get();
        let project = project_id(&cfg);
        let (code, remote) = open_tasks(&project).unwrap_or_else(|e| panic!("data: {}", api_err(e)));
        let remote = remote.unwrap_or_else(|| panic!("list data HTTP {code}"));
        println!("synced list: {} open tasks", remote.len());
        let mut matched = 0;
        for t in todos::all() {
            let Some(task) = t.tick_id.as_ref().and_then(|id| remote.get(id)) else { continue };
            matched += 1;
            let due = parse_due(task["dueDate"].as_str());
            println!(
                "  linked to-do: remote due {:?} → {:?} (stored {:?}), allDay {:?}/{} prio {:?}/{} same={}",
                task["dueDate"].as_str(),
                due,
                t.due,
                task["isAllDay"].as_bool(),
                t.due_all_day,
                task["priority"].as_i64(),
                t.tick_priority,
                !differs(&t, task)
            );
            // What we'd send back for this due date matches what TickTick has.
            let mut body = json!({});
            add_due(&mut body, &t);
            if let (Some(sent), Some(have)) = (body["dueDate"].as_str(), task["dueDate"].as_str()) {
                println!("    would send dueDate {sent} (TickTick has {have}), timeZone {}", body["timeZone"]);
                assert_eq!(todos::parse_iso(sent).map(|p| p.0 - p.2.unwrap()), todos::parse_iso(have).map(|p| p.0 - p.2.unwrap()));
            }
        }
        println!("{matched} local to-dos linked to open TickTick tasks");
    }
}
