//! Pomodoro, countdown and stopwatch. Time is measured from Instants, so nothing ticks in the background;
//! the UI reads the remaining time when it draws.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Pomodoro,
    Countdown,
    Stopwatch,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Focus,
    Break,
}

#[derive(Clone, Copy, Debug)]
pub struct Timer {
    pub kind: Kind,
    /// Total length (countdown / pomodoro phase); zero for the stopwatch.
    pub length: Duration,
    /// Running since (None = paused).
    started: Option<Instant>,
    /// Time already run before the last pause.
    banked: Duration,
    pub phase: Phase,
    pub rounds: u32,
}

impl Timer {
    pub fn elapsed(&self) -> Duration {
        self.banked + self.started.map_or(Duration::ZERO, |s| s.elapsed())
    }
    pub fn running(&self) -> bool {
        self.started.is_some()
    }
    pub fn remaining(&self) -> Duration {
        self.length.saturating_sub(self.elapsed())
    }
    /// 0..1 progress through the current length (stopwatch: through the current minute).
    pub fn progress(&self) -> f32 {
        if self.kind == Kind::Stopwatch {
            (self.elapsed().as_secs_f32() % 60.0) / 60.0
        } else if self.length.is_zero() {
            0.0
        } else {
            (self.elapsed().as_secs_f32() / self.length.as_secs_f32()).min(1.0)
        }
    }
    pub fn label(&self) -> &'static str {
        match (self.kind, self.phase) {
            (Kind::Pomodoro, Phase::Focus) => "Focus",
            (Kind::Pomodoro, Phase::Break) => "Break",
            (Kind::Countdown, _) => "Timer",
            (Kind::Stopwatch, _) => "Stopwatch",
        }
    }
    /// "12:34" (or h:mm:ss) — remaining for timers, elapsed for the stopwatch.
    pub fn text(&self) -> String {
        let d = if self.kind == Kind::Stopwatch { self.elapsed() } else { self.remaining() };
        let s = d.as_secs() + if self.kind == Kind::Stopwatch { 0 } else { u64::from(d.subsec_millis() > 0) };
        if s >= 3600 {
            format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        } else {
            format!("{:02}:{:02}", s / 60, s % 60)
        }
    }
}

pub struct Settings {
    pub focus_min: u64,
    pub break_min: u64,
}

static STATE: Mutex<Option<Timer>> = Mutex::new(None);
static CONFIG: Mutex<Settings> = Mutex::new(Settings { focus_min: 25, break_min: 5 });

pub fn current() -> Option<Timer> {
    *STATE.lock().unwrap()
}

pub fn config() -> (u64, u64) {
    let c = CONFIG.lock().unwrap();
    (c.focus_min, c.break_min)
}

pub fn set_config(focus_min: u64, break_min: u64) {
    let mut c = CONFIG.lock().unwrap();
    c.focus_min = focus_min.clamp(1, 180);
    c.break_min = break_min.clamp(1, 60);
}

pub fn start(kind: Kind, minutes: u64) {
    let length = match kind {
        Kind::Stopwatch => Duration::ZERO,
        Kind::Pomodoro => Duration::from_secs(config().0 * 60),
        Kind::Countdown => Duration::from_secs(minutes.max(1) * 60),
    };
    *STATE.lock().unwrap() =
        Some(Timer { kind, length, started: Some(Instant::now()), banked: Duration::ZERO, phase: Phase::Focus, rounds: 0 });
}

pub fn toggle_pause() {
    if let Some(t) = STATE.lock().unwrap().as_mut() {
        match t.started.take() {
            Some(s) => t.banked += s.elapsed(),
            None => t.started = Some(Instant::now()),
        }
    }
}

pub fn stop() {
    *STATE.lock().unwrap() = None;
}

/// Adds a minute to a running countdown.
pub fn add_minute() {
    if let Some(t) = STATE.lock().unwrap().as_mut() {
        if t.kind != Kind::Stopwatch {
            t.length += Duration::from_secs(60);
        }
    }
}

/// Pomodoro: moves to the next phase now (focus → break → focus), without an alert.
pub fn skip() {
    let mut st = STATE.lock().unwrap();
    let Some(t) = st.as_mut() else { return };
    if t.kind != Kind::Pomodoro {
        return;
    }
    let (focus, brk) = config();
    if t.phase == Phase::Focus {
        t.rounds += 1;
        t.phase = Phase::Break;
        t.length = Duration::from_secs(brk * 60);
    } else {
        t.phase = Phase::Focus;
        t.length = Duration::from_secs(focus * 60);
    }
    t.banked = Duration::ZERO;
    t.started = Some(Instant::now());
}

/// Checks for a finished timer: returns its label when one just ran out (a pomodoro moves to its next phase).
pub fn poll() -> Option<String> {
    let mut st = STATE.lock().unwrap();
    let t = st.as_mut()?;
    if t.kind == Kind::Stopwatch || !t.running() || t.remaining() > Duration::ZERO {
        return None;
    }
    match t.kind {
        Kind::Pomodoro => {
            let (focus, brk) = config();
            let msg = if t.phase == Phase::Focus { "Focus done · take a break" } else { "Break over · back to focus" };
            if t.phase == Phase::Focus {
                t.rounds += 1;
                t.phase = Phase::Break;
                t.length = Duration::from_secs(brk * 60);
            } else {
                t.phase = Phase::Focus;
                t.length = Duration::from_secs(focus * 60);
            }
            t.banked = Duration::ZERO;
            t.started = Some(Instant::now());
            Some(msg.to_string())
        }
        _ => {
            *st = None;
            Some("Timer finished".to_string())
        }
    }
}
