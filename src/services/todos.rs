//! The to-do list in todos.json (plus todos-deleted.json: TickTick tasks still to delete/complete there),
//! in the exact schema the .NET build writes, so both builds share the list.
//!
//! Dates are kept as the strings .NET writes ("2026-11-14T00:00:00+07:00", local time with its offset), so
//! a load and save changes nothing; `Todo::due_local` and friends read them.
//! Edits made here set NeedsPush so the next TickTick sync sends them (see ticktick.rs).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::Time::{SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime};

use super::{settings, store};

const FILE: &str = "todos.json";
const DELETED_FILE: &str = "todos-deleted.json";

/// TickTick priorities.
pub const PRIORITY_NONE: i32 = 0;
pub const PRIORITY_LOW: i32 = 1;
pub const PRIORITY_MEDIUM: i32 = 3;
pub const PRIORITY_HIGH: i32 = 5;

/// A due date as the UI gives it: (year, month, day, optional (hour, minute)); no time = all day.
pub type DueInput = (i32, u32, u32, Option<(u32, u32)>);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Todo {
    /// Runtime id for the UI (not saved: the .NET schema has none). Stable while the app runs.
    #[serde(skip)]
    pub id: String,
    /// Bumped on every local edit, so a sync running meanwhile doesn't overwrite it.
    #[serde(skip)]
    pub(crate) rev: u64,

    /// .NET DateTime, local with offset.
    #[serde(default)]
    pub created: String,
    /// TickTick task id, once the task exists there.
    #[serde(default)]
    pub tick_id: Option<String>,
    #[serde(default)]
    pub tick_project: Option<String>,
    /// Changed here since the last sync: send it to TickTick.
    #[serde(default)]
    pub needs_push: bool,
    /// Its completion has reached TickTick already.
    #[serde(default)]
    pub synced_done: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// Starred = TickTick high priority.
    #[serde(default)]
    pub starred: bool,
    /// .NET DateTime string, local with offset; all-day ones at local midnight.
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub due_all_day: bool,
    /// 0 none, 1 low, 3 medium, 5 high.
    #[serde(default)]
    pub tick_priority: i32,

    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Todo {
    fn new(text: &str) -> Todo {
        Todo {
            id: next_id(),
            rev: 0,
            created: now_string(),
            tick_id: None,
            tick_project: None,
            needs_push: false,
            synced_done: false,
            text: text.to_string(),
            done: false,
            starred: false,
            due: None,
            due_all_day: false,
            tick_priority: 0,
            extra: Map::new(),
        }
    }

    /// The due date in this PC's local time: (y, m, d, hour, minute).
    pub fn due_local(&self) -> Option<(i32, u32, u32, u32, u32)> {
        let secs = parse_local(self.due.as_deref()?)?.0;
        let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
        let t = secs.rem_euclid(86_400);
        Some((y, m, d, (t / 3600) as u32, (t % 3600 / 60) as u32))
    }

    /// "Today", "Tomorrow 09:30", "Sat 14 Nov", "Mon 16 Nov 18:00" (empty without a due date).
    pub fn due_text(&self) -> String {
        let Some((y, m, d, h, min)) = self.due_local() else { return String::new() };
        let day = days_from_civil(y, m, d);
        let today = local_now_secs().div_euclid(86_400);
        let time = if self.due_all_day { String::new() } else { format!(" {h:02}:{min:02}") };
        if day == today {
            format!("Today{time}")
        } else if day == today + 1 {
            format!("Tomorrow{time}")
        } else {
            const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{} {} {}{time}", DAYS[(day + 4).rem_euclid(7) as usize], d, MONTHS[(m - 1) as usize])
        }
    }

    /// Open and past its due day (all day) or time.
    pub fn overdue(&self) -> bool {
        if self.done {
            return false;
        }
        let Some(due) = self.due.as_deref().and_then(parse_local) else { return false };
        let now = local_now_secs();
        if self.due_all_day {
            due.0.div_euclid(86_400) < now.div_euclid(86_400)
        } else {
            due.0 < now
        }
    }

    /// Due today (or overdue and open).
    pub fn due_today(&self) -> bool {
        self.due.as_deref().and_then(parse_local).map_or(false, |d| {
            d.0.div_euclid(86_400) <= local_now_secs().div_euclid(86_400)
        })
    }
}

/// A TickTick task removed here: to be deleted there, or (`complete`) completed there.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TickRef {
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub complete: bool,
}

pub(crate) struct Store {
    pub items: Vec<Todo>,
    pub deleted: Vec<TickRef>,
    /// When the user last changed something (TickTick syncs a few seconds after).
    pub last_edit: Option<Instant>,
}

fn next_id() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("t{}", N.fetch_add(1, Ordering::Relaxed))
}

fn cell() -> &'static Mutex<Store> {
    static CELL: OnceLock<Mutex<Store>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut items: Vec<Todo> = store::load(FILE);
        for t in &mut items {
            t.id = next_id();
            if t.created.is_empty() {
                t.created = now_string(); // .NET can't read an empty date
            }
        }
        Mutex::new(Store { items, deleted: store::load(DELETED_FILE), last_edit: None })
    })
}

fn listeners() -> &'static Mutex<Vec<fn()>> {
    static L: OnceLock<Mutex<Vec<fn()>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(Vec::new()))
}

/// Called after any change (by the user or a sync), on the thread that made it.
pub fn on_change(cb: fn()) {
    listeners().lock().unwrap_or_else(|e| e.into_inner()).push(cb);
}

fn notify() {
    let list = listeners().lock().unwrap_or_else(|e| e.into_inner()).clone();
    for cb in list {
        cb();
    }
}

fn save(s: &Store) {
    store::save(FILE, &s.items);
    store::save(DELETED_FILE, &s.deleted);
}

/// Runs `f` on the store, then saves and notifies. `user` marks it as the user's edit (sync follows).
pub(crate) fn with_store<R>(user: bool, f: impl FnOnce(&mut Store) -> R) -> R {
    let r = {
        let mut s = cell().lock().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut s);
        if user {
            s.last_edit = Some(Instant::now());
        }
        save(&s);
        r
    };
    notify();
    r
}

/// Copies of the items and pending TickTick deletions, without saving.
pub(crate) fn snapshot() -> (Vec<Todo>, Vec<TickRef>) {
    let s = cell().lock().unwrap_or_else(|e| e.into_inner());
    (s.items.clone(), s.deleted.clone())
}

/// A new to-do (with a runtime id) not yet in the list; sync fills it and pushes it.
pub(crate) fn add_detached() -> Todo {
    Todo::new("")
}

/// Edits one to-do as the user. `push` = the change should go to TickTick.
fn edit(id: &str, push: bool, f: impl FnOnce(&mut Todo)) {
    with_store(true, |s| {
        if let Some(t) = s.items.iter_mut().find(|t| t.id == id) {
            f(t);
            t.rev += 1;
            if push {
                t.needs_push = true;
            }
        }
    });
}

// ---------------------------------------------------------------- public API

/// All to-dos, in stored order (oldest first).
pub fn all() -> Vec<Todo> {
    cell().lock().unwrap_or_else(|e| e.into_inner()).items.clone()
}

pub fn get(id: &str) -> Option<Todo> {
    cell().lock().unwrap_or_else(|e| e.into_inner()).items.iter().find(|t| t.id == id).cloned()
}

/// The due date a new to-do gets from Settings → To-Dos ("today" or "none").
pub fn default_due() -> Option<DueInput> {
    if settings::get().todo_default_due != "today" {
        return None;
    }
    let (y, m, d) = civil_from_days(local_now_secs().div_euclid(86_400));
    Some((y, m, d, None))
}

/// Adds a to-do. Priority 5 (high) also stars it.
pub fn add(text: &str, priority: i32, due: Option<DueInput>) -> Todo {
    let mut t = Todo::new(text.trim());
    t.tick_priority = priority;
    t.starred = priority >= PRIORITY_HIGH;
    set_due_fields(&mut t, due);
    let copy = t.clone();
    with_store(true, |s| s.items.push(t));
    copy
}

pub fn set_done(id: &str, done: bool) {
    edit(id, false, |t| {
        if t.done == done {
            return;
        }
        t.done = done;
        if !done && t.synced_done {
            // Already completed in TickTick: it comes back as a new open task there.
            t.synced_done = false;
            t.tick_id = None;
            t.tick_project = None;
        }
    });
}

pub fn set_text(id: &str, text: &str) {
    edit(id, true, |t| t.text = text.trim().to_string());
}

pub fn set_starred(id: &str, starred: bool) {
    edit(id, true, |t| {
        t.starred = starred;
        if starred {
            t.tick_priority = PRIORITY_HIGH;
        } else if t.tick_priority >= PRIORITY_HIGH {
            t.tick_priority = PRIORITY_NONE;
        }
    });
}

/// 0 none, 1 low, 3 medium, 5 high (= starred).
pub fn set_priority(id: &str, priority: i32) {
    edit(id, true, |t| {
        t.tick_priority = priority;
        t.starred = priority >= PRIORITY_HIGH;
    });
}

pub fn set_due(id: &str, due: Option<DueInput>) {
    edit(id, true, |t| set_due_fields(t, due));
}

/// Removes a to-do; a linked TickTick task is deleted there on the next sync.
pub fn delete(id: &str) {
    with_store(true, |s| {
        if let Some(i) = s.items.iter().position(|t| t.id == id) {
            let t = s.items.remove(i);
            if let (Some(tid), false) = (t.tick_id, t.synced_done) {
                s.deleted.push(TickRef { id: tid, project_id: t.tick_project.unwrap_or_default(), complete: false });
            }
        }
    });
}

/// Removes ticked-off to-dos; ones not yet completed in TickTick still are, on the next sync.
pub fn clear_done() {
    with_store(true, |s| {
        let (done, open): (Vec<Todo>, Vec<Todo>) = s.items.drain(..).partition(|t| t.done);
        s.items = open;
        for t in done {
            if let (Some(tid), false) = (t.tick_id, t.synced_done) {
                s.deleted.push(TickRef { id: tid, project_id: t.tick_project.unwrap_or_default(), complete: true });
            }
        }
    });
}

/// Something here hasn't reached TickTick yet, and when the user last changed something.
pub fn pending_sync() -> (bool, Option<Instant>) {
    let s = cell().lock().unwrap_or_else(|e| e.into_inner());
    let pending = !s.deleted.is_empty()
        || s.items.iter().any(|t| {
            (t.tick_id.is_none() && !t.done) || (t.tick_id.is_some() && (t.needs_push || (t.done && !t.synced_done)))
        });
    (pending, s.last_edit)
}

fn set_due_fields(t: &mut Todo, due: Option<DueInput>) {
    match due {
        None => {
            t.due = None;
            t.due_all_day = false;
        }
        Some((y, m, d, time)) => {
            let (h, min) = time.unwrap_or((0, 0));
            let local = days_from_civil(y, m, d) * 86_400 + h as i64 * 3600 + min as i64 * 60;
            t.due = Some(format_local(local, 0));
            t.due_all_day = time.is_none();
        }
    }
}

// ---------------------------------------------------------------- dates
//
// "Local seconds" = the local wall-clock time counted as if it were UTC seconds since 1970, which makes
// day/date arithmetic plain integer math. Conversions to and from real UTC go through Windows' time zone.

pub(crate) fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64 - (m <= 2) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + (m <= 2) as i64) as i32, m, d)
}

fn to_systemtime(secs: i64) -> SYSTEMTIME {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDayOfWeek: 0,
        wDay: d as u16,
        wHour: (t / 3600) as u16,
        wMinute: (t % 3600 / 60) as u16,
        wSecond: (t % 60) as u16,
        wMilliseconds: 0,
    }
}

fn from_systemtime(st: &SYSTEMTIME) -> i64 {
    days_from_civil(st.wYear as i32, st.wMonth as u32, st.wDay as u32) * 86_400
        + st.wHour as i64 * 3600
        + st.wMinute as i64 * 60
        + st.wSecond as i64
}

/// Real UTC seconds → local seconds.
pub(crate) fn local_from_utc(utc: i64) -> i64 {
    let st = to_systemtime(utc);
    let mut out = SYSTEMTIME::default();
    match unsafe { SystemTimeToTzSpecificLocalTime(None, &st, &mut out) } {
        Ok(()) => from_systemtime(&out),
        Err(_) => utc,
    }
}

/// Local seconds → real UTC seconds.
pub(crate) fn utc_from_local(local: i64) -> i64 {
    let st = to_systemtime(local);
    let mut out = SYSTEMTIME::default();
    match unsafe { TzSpecificLocalTimeToSystemTime(None, &st, &mut out) } {
        Ok(()) => from_systemtime(&out),
        Err(_) => local,
    }
}

/// Now in UTC: (seconds, 100 ns ticks).
fn now_utc() -> (i64, u32) {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos() / 100)
}

pub(crate) fn local_now_secs() -> i64 {
    local_from_utc(now_utc().0)
}

fn now_string() -> String {
    let (utc, ticks) = now_utc();
    format_local(local_from_utc(utc), ticks)
}

/// Local seconds as .NET writes a local DateTime: "2026-11-13T23:47:43.0951478+07:00".
pub(crate) fn format_local(local: i64, ticks: u32) -> String {
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let t = local.rem_euclid(86_400);
    let mut s = format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", t / 3600, t % 3600 / 60, t % 60);
    if ticks > 0 {
        let frac = format!("{ticks:07}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    let off = (local - utc_from_local(local)) / 60;
    let sign = if off < 0 { '-' } else { '+' };
    s.push_str(&format!("{sign}{:02}:{:02}", off.abs() / 60, off.abs() % 60));
    s
}

/// Parses an ISO 8601 date-time ("…Z", "…+07:00", "…+0000" or no offset = already local) into
/// (local seconds, 100 ns ticks).
pub(crate) fn parse_local(text: &str) -> Option<(i64, u32)> {
    let (naive, ticks, offset) = parse_iso(text)?;
    Some(match offset {
        Some(off) => (local_from_utc(naive - off), ticks),
        None => (naive, ticks),
    })
}

/// Parses an ISO 8601 date-time into (its own wall-clock seconds, 100 ns ticks, offset in seconds if given).
pub(crate) fn parse_iso(text: &str) -> Option<(i64, u32, Option<i64>)> {
    let s = text.trim();
    let num = |a: usize, b: usize| -> Option<i64> {
        let p = s.get(a..b)?;
        p.bytes().all(|c| c.is_ascii_digit()).then(|| p.parse().ok())?
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if s.as_bytes().get(4) != Some(&b'-') || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y as i32, m as u32, d as u32) * 86_400;
    let mut ticks = 0u32;
    let mut rest = s.get(10..)?;
    if let Some(r) = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' ')) {
        let h = r.get(0..2)?.parse::<i64>().ok()?;
        let min = r.get(3..5)?.parse::<i64>().ok()?;
        secs += h * 3600 + min * 60;
        rest = &r[5..];
        if let Some(r) = rest.strip_prefix(':') {
            secs += r.get(0..2)?.parse::<i64>().ok()?;
            rest = &r[2..];
            if let Some(r) = rest.strip_prefix('.') {
                let n = r.bytes().take_while(u8::is_ascii_digit).count();
                let digits = &r[..n];
                let mut padded = digits.chars().take(7).collect::<String>();
                while padded.len() < 7 {
                    padded.push('0');
                }
                ticks = padded.parse().ok()?;
                rest = &r[n..];
            }
        }
    }
    let offset = match rest {
        "" => None,
        "Z" | "z" => Some(0),
        o => {
            let sign = match o.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let digits: String = o[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 {
                return None;
            }
            let h: i64 = digits[..2].parse().ok()?;
            let mm: i64 = digits[2..].parse().ok()?;
            Some(sign * (h * 3600 + mm * 60))
        }
    };
    Some((secs, ticks, offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::settings::tests::{read_json, same, scratch};

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(days_from_civil(2026, 11, 14)), (2026, 11, 14));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        let (s, t, o) = parse_iso("2019-11-13T03:00:00.000+0000").unwrap();
        assert_eq!((s, t, o), (days_from_civil(2019, 11, 13) * 86_400 + 3 * 3600, 0, Some(0)));
        let (_, t, o) = parse_iso("2026-11-13T23:47:43.0951478+07:00").unwrap();
        assert_eq!((t, o), (951_478, Some(7 * 3600)));
        // Local round trip through the string form.
        let local = days_from_civil(2026, 11, 14) * 86_400;
        assert_eq!(parse_local(&format_local(local, 0)).unwrap().0, local);
        let f = format_local(local, 951_470);
        assert!(f.contains(".095147+") || f.contains(".095147-"), "{f}");
    }

    #[test]
    #[ignore]
    fn todos_round_trip() {
        let dir = scratch();
        let before = read_json(&dir.join(FILE));
        let before_deleted = read_json(&dir.join(DELETED_FILE));
        let items = all();
        println!("{} to-dos, {} pending deletions", items.len(), cell().lock().unwrap().deleted.len());
        for t in &items {
            println!(
                "  [{}] due={:?} allDay={} text={} chars → \"{}\" overdue={} prio={} linked={}",
                if t.done { 'x' } else { ' ' },
                t.due_local(),
                t.due_all_day,
                t.text.chars().count(),
                t.due_text(),
                t.overdue(),
                t.tick_priority,
                t.tick_id.is_some()
            );
            // The written due string is what we would produce for the same local time.
            if let (Some(d), Some((y, m, dd, h, min))) = (&t.due, t.due_local()) {
                let again = format_local(days_from_civil(y, m, dd) * 86_400 + h as i64 * 3600 + min as i64 * 60, 0);
                assert_eq!(&again, d);
            }
        }
        with_store(false, |_| {}); // save unchanged
        let mut diffs = vec![];
        same(&before, &read_json(&dir.join(FILE)), "todos", &mut diffs);
        same(&before_deleted, &read_json(&dir.join(DELETED_FILE)), "deleted", &mut diffs);
        for d in &diffs {
            println!("DIFF {d}");
        }
        assert!(diffs.is_empty());

        // Add / edit / delete on the scratch copy, then check the file still parses as the .NET schema.
        let t = add("Rust test item", PRIORITY_HIGH, Some((2026, 12, 1, Some((9, 30)))));
        set_text(&t.id, "Rust test item (edited)");
        set_priority(&t.id, PRIORITY_MEDIUM);
        let saved = read_json(&dir.join(FILE));
        let last = saved.as_array().unwrap().last().unwrap();
        println!("new item as saved: {}", serde_json::to_string(last).unwrap());
        assert_eq!(last["TickPriority"], 3);
        assert_eq!(last["Starred"], false);
        assert!(last["Due"].as_str().unwrap().starts_with("2026-12-01T09:30:00"));
        delete(&t.id);
        assert_eq!(all().len(), items.len());
    }
}
