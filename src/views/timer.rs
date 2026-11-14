//! Timers: Timer | Stopwatch | Pomodoro switch on top; below, one card with the round progress ring and the
//! time, what the mode is doing (session dots, preset, laps), and the controls: start/pause, a secondary
//! button (+1 min for timers, lap for the stopwatch), reset, plus the length and sound pickers.

use std::time::Duration;

use crate::gfx::{color, with_alpha, Align, BOLD, MEDIUM, SEMIBOLD};
use crate::services::{settings, timers};
use crate::services::timers::Kind;
use crate::ui::{id, Rect, Ui};

const SOUNDS: [&str; 4] = ["No Sound", "Soft Chime", "Bell", "Ping"];
const POMODORO: [(u64, u64); 4] = [(15, 3), (25, 5), (45, 10), (60, 15)];
const PRESETS: [u64; 8] = [1, 5, 10, 15, 25, 30, 45, 60];
/// C# "GoodBrush" (the break arc).
const GOOD: u32 = 0x32D74B;
const ROW_H: f32 = 30.0;

/// An open picker list. It opens upward when there's no room below (the content area ends just under
/// the boxes) and scrolls when it's taller than the room.
struct Popup {
    id: u64,
    anchor: Rect,
    options: Vec<String>,
    selected: usize,
    scroll: f32,
}

pub struct State {
    mode: Kind,
    /// Stopwatch laps (elapsed at each lap).
    laps: Vec<Duration>,
    popup: Option<Popup>,
    /// Picked this frame: (box id, option).
    picked: Option<(u64, usize)>,
}

impl Default for State {
    fn default() -> Self {
        Self { mode: Kind::Pomodoro, laps: Vec::new(), popup: None, picked: None }
    }
}

pub fn shown(s: &mut State) {
    s.popup = None;
    let st = settings::get();
    timers::set_config(st.pomodoro_work_min.max(1) as u64, st.pomodoro_break_min.max(1) as u64);
    // Open on whatever is running.
    if let Some(t) = timers::current() {
        s.mode = t.kind;
    }
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, _go: &mut Option<&'static str>) {
    let t = ui.t;
    // The open list takes clicks and the wheel before anything under it.
    popup_input(ui, s, r);
    let (seg, rest) = r.take_top(36.0, 12.0);
    segments(ui, s, seg);

    let card = rest.pad(18.0, 0.0, 18.0, 6.0);
    ui.card(card, 18.0);
    let inner = card.pad(18.0, 10.0, 18.0, 10.0);

    let st = settings::get();
    let cur = timers::current().filter(|x| x.kind == s.mode);
    let (focus_min, _) = timers::config();
    let timer_min = PRESETS.iter().copied().find(|m| *m == st.timer_minutes as u64).unwrap_or(10);
    let text = match (&cur, s.mode) {
        (Some(c), _) => c.text(),
        (None, Kind::Pomodoro) => format!("{focus_min:02}:00"),
        (None, Kind::Countdown) => format!("{timer_min:02}:00"),
        (None, Kind::Stopwatch) => "00:00".into(),
    };
    let progress = match (&cur, s.mode) {
        (Some(c), Kind::Pomodoro | Kind::Countdown) => c.progress(),
        _ => 0.0,
    };
    let is_break = cur.is_some_and(|c| c.phase == timers::Phase::Break);

    // Ring.
    let ring = Rect::new(inner.x, inner.cy() - 64.0, 128.0, 128.0);
    let (cx, cy) = (ring.cx(), ring.cy());
    ui.g.stroke_circle(cx, cy, 57.5, ui.c(t.track), 7.0);
    ui.g.fill_circle(cx, cy, 46.0, ui.c(with_alpha(t.accent_soft, 0.55)));
    let p = progress.clamp(0.0, 0.9999);
    if p > 0.001 {
        let arc_c = if is_break { color(GOOD, 1.0) } else { t.accent };
        ui.g.arc(cx, cy, 57.5, 0.0, p * std::f32::consts::TAU, ui.c(arc_c), 7.0);
    }
    ui.text(&text, Rect::new(ring.x - 10.0, ring.y, ring.w + 20.0, ring.h), if text.len() > 5 { 21.0 } else { 26.0 }, BOLD, t.text, Align::Center);

    // Controls (right): buttons row, then the two pickers.
    let ctrl_w = 254.0;
    let ctrl = Rect::new(inner.right() - ctrl_w, inner.cy() - 45.0, ctrl_w, 90.0);
    let running = cur.is_some_and(|c| c.running());
    let reset = Rect::new(ctrl.right() - 46.0, ctrl.y + 2.0, 46.0, 46.0);
    let second = Rect::new(reset.x - 10.0 - 46.0, reset.y, 46.0, 46.0);
    let primary = Rect::new(second.x - 10.0 - 50.0, ctrl.y, 50.0, 50.0);
    let hov = ui.hovered(primary);
    ui.g.fill_circle(primary.cx(), primary.cy(), 25.0, ui.c(with_alpha(t.accent, if hov { 0.85 } else { 1.0 })));
    ui.icon(if running { "Pause" } else { "Play" }, primary.cx() + if running { 0.0 } else { 1.5 }, primary.cy(), 19.0, t.on_accent);
    if ui.clicked(primary) {
        match cur {
            Some(_) => timers::toggle_pause(),
            None => {
                s.laps.clear();
                timers::start(s.mode, timer_min);
            }
        }
    }
    // C#: Skip (pomodoro), +1 min (timer), Lap (stopwatch).
    let second_icon = match s.mode {
        Kind::Stopwatch => "Flag",
        Kind::Pomodoro => "Forward",
        Kind::Countdown => "Plus",
    };
    if circle_button(ui, second, second_icon) {
        match (s.mode, &cur) {
            (Kind::Stopwatch, Some(c)) if c.running() => s.laps.push(c.elapsed()),
            (Kind::Stopwatch, _) => {}
            // Skip: straight to the next phase.
            (Kind::Pomodoro, _) => timers::skip(),
            (_, Some(_)) => timers::add_minute(),
            _ => {}
        }
    }
    if circle_button(ui, reset, "Refresh") {
        if cur.is_some() {
            timers::stop();
        }
        s.laps.clear();
    }

    let dur = Rect::new(ctrl.x, ctrl.bottom() - 28.0, 128.0, 28.0);
    let snd = Rect::new(dur.right() + 8.0, dur.y, 118.0, 28.0);
    let sounds: Vec<String> = SOUNDS.iter().map(|x| x.to_string()).collect();
    match s.mode {
        Kind::Pomodoro => {
            let opts: Vec<String> = POMODORO.iter().map(|(w, b)| format!("{w} / {b} min")).collect();
            let sel = POMODORO.iter().position(|&(w, b)| w == st.pomodoro_work_min as u64 && b == st.pomodoro_break_min as u64).unwrap_or(1);
            if let Some(i) = combo(ui, s, id("timer.duration.pomodoro"), dur, &opts, sel) {
                let (w, b) = POMODORO[i];
                settings::update(|x| {
                    x.pomodoro_work_min = w as i32;
                    x.pomodoro_break_min = b as i32;
                });
                timers::set_config(w, b);
            }
            let sel = SOUNDS.iter().position(|x| *x == st.pomodoro_sound).unwrap_or(1);
            if let Some(i) = combo(ui, s, id("timer.sound.pomodoro"), snd, &sounds, sel) {
                settings::update(|x| x.pomodoro_sound = SOUNDS[i].to_string());
            }
        }
        Kind::Countdown => {
            let opts: Vec<String> = PRESETS.iter().map(|m| format!("{m} min")).collect();
            let sel = PRESETS.iter().position(|m| *m == timer_min).unwrap_or(2);
            if let Some(i) = combo(ui, s, id("timer.duration.countdown"), dur, &opts, sel) {
                settings::update(|x| x.timer_minutes = PRESETS[i] as i32);
            }
            let sel = SOUNDS.iter().position(|x| *x == st.timer_sound).unwrap_or(1);
            if let Some(i) = combo(ui, s, id("timer.sound.countdown"), snd, &sounds, sel) {
                settings::update(|x| x.timer_sound = SOUNDS[i].to_string());
            }
        }
        Kind::Stopwatch => {
            disabled_box(ui, dur, "Laps");
            disabled_box(ui, snd, "No Sound");
        }
    }

    // Mode info (middle).
    let ix = ring.right() + 18.0;
    let iw = (ctrl.x - 12.0 - ix).max(40.0);
    match s.mode {
        Kind::Pomodoro => {
            let rounds = cur.map_or(0, |c| c.rounds);
            // C# stack (margin-top 10, centred): title 18, +3 session 15, +7 dots 7, +8 today 14.5.
            let y = inner.cy() - 31.0;
            title_row(ui, ix, y, iw, if is_break { "Moon" } else { "Bolt" }, if is_break { "Break" } else { "Focus" });
            ui.text(&format!("Session {} of 4", rounds % 4 + 1), Rect::new(ix, y + 20.0, iw, 17.0), 12.5, SEMIBOLD, t.subtext, Align::Left);
            let mut filled = rounds % 4;
            if rounds > 0 && filled == 0 {
                filled = 4;
            }
            for i in 0..4 {
                let c = if i < filled { t.accent } else { t.track };
                ui.g.fill_circle(ix + 3.5 + i as f32 * 13.0, y + 46.5, 3.5, ui.c(c));
            }
            let today = match rounds {
                0 => "No sessions yet today".to_string(),
                1 => "1 session today".to_string(),
                n => format!("{n} sessions today"),
            };
            ui.text(&today, Rect::new(ix, y + 57.0, iw, 16.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
        }
        Kind::Countdown => {
            let y = inner.cy() - 24.0;
            title_row(ui, ix, y, iw, "Clock", "Timer");
            ui.text(&format!("{timer_min} minute countdown"), Rect::new(ix, y + 20.0, iw, 17.0), 12.5, SEMIBOLD, t.subtext, Align::Left);
            ui.text("Stays in the closed notch.", Rect::new(ix, y + 43.0, iw, 16.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
        }
        Kind::Stopwatch => {
            let y = inner.cy() - 24.0;
            title_row(ui, ix, y, iw, "Clock", "Stopwatch");
            let laps = match s.laps.len() {
                0 => "No laps yet".to_string(),
                1 => "1 lap".to_string(),
                n => format!("{n} laps"),
            };
            ui.text(&laps, Rect::new(ix, y + 20.0, iw, 17.0), 12.5, SEMIBOLD, t.subtext, Align::Left);
            ui.text("Counts up from zero.", Rect::new(ix, y + 43.0, iw, 16.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
        }
    }
    s.picked = None;
    draw_popup(ui, s, r);
}

/// Where the open list goes: below the box if it fits in `area`, else above (or whichever side is bigger),
/// clipped to the room there.
fn popup_rect(p: &Popup, area: Rect) -> Rect {
    let full = ROW_H * p.options.len() as f32 + 8.0;
    let below = area.bottom() - p.anchor.bottom() - 4.0;
    let above = p.anchor.y - 4.0 - area.y;
    if full <= below || below >= above {
        Rect::new(p.anchor.x, p.anchor.bottom() + 4.0, p.anchor.w, full.min(below))
    } else {
        let h = full.min(above);
        Rect::new(p.anchor.x, p.anchor.y - 4.0 - h, p.anchor.w, h)
    }
}

fn popup_input(ui: &mut Ui, s: &mut State, area: Rect) {
    s.picked = None;
    let Some(p) = s.popup.as_mut() else { return };
    let list = popup_rect(p, area);
    let max = (ROW_H * p.options.len() as f32 + 8.0 - list.h).max(0.0);
    if ui.input.wheel != 0.0 && list.contains(ui.mouse()) {
        p.scroll = (p.scroll - ui.input.wheel * ROW_H).clamp(0.0, max);
        ui.input.wheel = 0.0;
    }
    if ui.input.keys.contains(&crate::ui::Key::Escape) {
        s.popup = None;
        return;
    }
    if !ui.input.pressed || ui.input.consumed {
        return;
    }
    let m = ui.mouse();
    if list.contains(m) {
        let i = ((m.1 - list.y - 4.0 + p.scroll) / ROW_H).floor();
        if i >= 0.0 && (i as usize) < p.options.len() {
            s.picked = Some((p.id, i as usize));
        }
        ui.input.consumed = true;
    } else if p.anchor.contains(m) {
        ui.input.consumed = true; // clicking the box again closes it
    }
    s.popup = None;
}

fn draw_popup(ui: &mut Ui, s: &mut State, area: Rect) {
    let Some(p) = s.popup.as_ref() else { return };
    let t = ui.t;
    let list = popup_rect(p, area);
    ui.fill(list, 8.0, t.surface2);
    ui.g.stroke_round(list.inset(0.5, 0.5).d2d(), 7.5, ui.c(t.stroke), 1.0);
    ui.push_clip(list.inset(1.0, 4.0));
    for (i, o) in p.options.iter().enumerate() {
        let row = Rect::new(list.x + 4.0, list.y + 4.0 + i as f32 * ROW_H - p.scroll, list.w - 8.0, ROW_H);
        if row.bottom() < list.y || row.y > list.bottom() {
            continue;
        }
        if row.contains(ui.mouse()) && list.contains(ui.mouse()) {
            ui.fill(row, 6.0, t.hover);
        }
        let sel = i == p.selected;
        ui.text(o, row.pad(10.0, 0.0, 26.0, 0.0), 13.0, if sel { SEMIBOLD } else { MEDIUM }, if sel { t.text } else { t.subtext }, Align::Left);
        if sel {
            ui.icon("Check", row.right() - 14.0, row.cy(), 12.0, t.text);
        }
    }
    ui.pop_clip();
    let full = ROW_H * p.options.len() as f32 + 8.0;
    if full > list.h {
        let bar_h = (list.h * list.h / full).max(18.0);
        let y = list.y + (list.h - bar_h) * (p.scroll / (full - list.h));
        ui.fill(Rect::new(list.right() - 4.0, y, 2.0, bar_h), 1.0, with_alpha(t.text, 0.25));
    }
}

fn segments(ui: &mut Ui, s: &mut State, r: Rect) {
    let t = ui.t;
    let labels = [("Timer", Kind::Countdown), ("Stopwatch", Kind::Stopwatch), ("Pomodoro", Kind::Pomodoro)];
    let widths: Vec<f32> = labels.iter().map(|(l, _)| (ui.measure(l, 13.5, SEMIBOLD) + 28.0).max(104.0)).collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0;
    let bar = Rect::new(r.cx() - total / 2.0, r.y, total, 36.0);
    ui.fill(bar, 11.0, t.surface);
    let mut x = bar.x + 3.0;
    for ((label, kind), w) in labels.iter().zip(widths) {
        let b = Rect::new(x, bar.y + 3.0, w, 30.0);
        let on = s.mode == *kind;
        let hov = ui.hovered(b);
        if on {
            ui.fill(b, 8.0, t.accent_soft);
        }
        let fg = if on || hov { t.text } else { t.subtext };
        ui.text(label, b, 13.5, if on { SEMIBOLD } else { MEDIUM }, fg, Align::Center);
        if ui.clicked(b) {
            s.mode = *kind;
        }
        x += w;
    }
}

fn circle_button(ui: &mut Ui, r: Rect, icon: &'static str) -> bool {
    let t = ui.t;
    let hov = ui.hovered(r);
    ui.g.fill_circle(r.cx(), r.cy(), r.w / 2.0, ui.c(if hov { t.hover } else { t.surface2 }));
    ui.icon(icon, r.cx(), r.cy(), 18.0, t.text);
    ui.clicked(r)
}

fn title_row(ui: &mut Ui, x: f32, y: f32, w: f32, icon: &'static str, label: &str) {
    let t = ui.t;
    ui.icon(icon, x + 8.5, y + 10.0, 17.0, t.text);
    ui.text(label, Rect::new(x + 24.0, y, w - 24.0, 20.0), 15.0, SEMIBOLD, t.text, Align::Left);
}

/// Stopwatch: the boxes stay (C# disables them, which looks the same) but do nothing.
fn disabled_box(ui: &mut Ui, r: Rect, label: &str) {
    combo_face(ui, r, label, false);
}

/// The C# combo box (Surface2, radius 7, 13 SemiBold, a thin chevron; hover dims to the hover brush) over
/// the kit dropdown, which still does the picking and draws the open list.
fn combo(ui: &mut Ui, s: &mut State, cid: u64, r: Rect, options: &[String], selected: usize) -> Option<usize> {
    let picked = s.picked.filter(|(p, _)| *p == cid).map(|(_, i)| i);
    let open = s.popup.as_ref().is_some_and(|p| p.id == cid);
    if !open && ui.clicked(r) {
        s.popup = Some(Popup { id: cid, anchor: r, options: options.to_vec(), selected, scroll: 0.0 });
        ui.animate = true;
    }
    let hov = ui.hovered(r) || open;
    combo_face(ui, r, options.get(selected).map(String::as_str).unwrap_or(""), hov);
    picked
}

/// The box itself; hover swaps Surface2 for the (translucent) hover brush over the card.
fn combo_face(ui: &mut Ui, r: Rect, label: &str, hov: bool) {
    let t = ui.t;
    if hov {
        ui.fill(r, 7.0, t.surface);
        ui.fill(r, 7.0, t.hover);
    } else {
        ui.fill(r, 7.0, t.surface2);
    }
    ui.text(label, r.pad(9.0, 0.0, 26.0, 0.0), 13.0, SEMIBOLD, t.text, Align::Left);
    let (cx, cy) = (r.right() - 10.0 - 4.0, r.cy());
    let c = ui.c(t.subtext);
    ui.g.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, c, 1.4);
    ui.g.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, c, 1.4);
}
