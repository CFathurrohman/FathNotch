//! Prayer: the next prayer with a live countdown and a progress bar since the last one on the left; on the
//! right the rolling timeline as the sun's path: a straight horizon through the middle, the day prayers
//! arching above it and the night below, the current position as a glowing dot. The timeline starts at the
//! prayer we're in (at Isya: Isya, Subuh, Dzuhur, Ashar, Maghrib) and slides one step when a prayer comes.

use std::time::Instant;

use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN};

use crate::gfx::{pt, with_alpha, Align, BOLD, REGULAR, SEMIBOLD};
use crate::model;
use crate::services::prayer::{self, NAMES};
use crate::services::{calendar, settings};
use crate::ui::{ButtonStyle, Rect, Ui};

const SLOTS: usize = 5;

/// Day, latitude, longitude, method, Hanafi Asr: what the cached neighbour days were computed for.
type PlaceKey = (i64, Option<f64>, Option<f64>, String, bool);

#[derive(Default)]
pub struct State {
    shown_at: Option<Instant>,
    /// The prayer the timeline started at when last drawn (index, absolute hours).
    first: Option<(usize, i64)>,
    slide_at: Option<Instant>,
    /// Yesterday's and tomorrow's times, keyed by today and the place/method.
    cache: Option<(PlaceKey, [f64; 6], [f64; 6])>,
}

pub fn shown(s: &mut State) {
    s.shown_at = Some(Instant::now());
    s.slide_at = None;
}

/// Where each prayer sits on the sun's path, as a fraction of a half sine (0 = rising horizon, 0.5 = noon,
/// 1 = setting horizon; past 1 is night).
fn sun_phase(i: usize) -> f64 {
    match i {
        0 => -0.08,
        2 => 0.5,
        3 => 0.72,
        4 => 1.0,
        _ => 1.1,
    }
}

fn times_on(day: i64) -> Option<[f64; 6]> {
    let s = settings::shared();
    let (lat, lon) = (s.prayer_lat?, s.prayer_lon?);
    let (y, m, d) = calendar::civil_from_days(day);
    let tz = prayer::tz_hours_on(y, m, d);
    let t = prayer::compute_with(y, m, d, lat, lon, tz, prayer::method(&s.prayer_method), if s.prayer_asr_hanafi { 2.0 } else { 1.0 });
    Some(model::times_list(&t))
}

/// (prayer index, time in hours relative to today's midnight).
type Slot = (usize, f64);

fn timeline(s: &mut State, today: [f64; 6], now: f64) -> Vec<Slot> {
    let (y, m, d) = calendar::today();
    let day = calendar::days_from_civil(y, m, d);
    let st = settings::shared();
    let same = s.cache.as_ref().is_some_and(|(k, _, _)| {
        k.0 == day && k.1 == st.prayer_lat && k.2 == st.prayer_lon && k.3 == st.prayer_method && k.4 == st.prayer_asr_hanafi
    });
    if !same {
        let key = (day, st.prayer_lat, st.prayer_lon, st.prayer_method.clone(), st.prayer_asr_hanafi);
        let prev = times_on(day - 1).unwrap_or(today);
        let next = times_on(day + 1).unwrap_or(today);
        s.cache = Some((key, prev, next));
    }
    let (prev, next) = s.cache.as_ref().map(|c| (c.1, c.2)).unwrap();
    let mut all = Vec::with_capacity(15);
    for (off, list) in [(-24.0, prev), (0.0, today), (24.0, next)] {
        for i in [0, 2, 3, 4, 5] {
            if list[i].is_finite() {
                all.push((i, list[i] + off));
            }
        }
    }
    let cur = all.iter().rposition(|p| p.1 <= now).unwrap_or(0);
    all.into_iter().skip(cur).take(SLOTS).collect()
}

fn span(hours: f64) -> String {
    let mins = hours * 60.0;
    if mins < 1.0 {
        return format!("{} s", (hours * 3600.0).max(0.0) as i64);
    }
    let mut h = (mins / 60.0).floor() as i64;
    let mut m = (mins - h as f64 * 60.0).ceil() as i64;
    if m == 60 {
        h += 1;
        m = 0;
    }
    match (h, m) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Back-ease (a little overshoot), like WPF's BackEase amplitude 0.6.
fn pop(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let u = 1.0 - t;
    1.0 - (u * u * u - 0.6 * u * (u * std::f32::consts::PI).sin())
}

pub fn draw(ui: &mut Ui, s: &mut State, r: Rect, go: &mut Option<&'static str>) {
    let t = ui.t;
    let st = settings::shared();
    let times = model::prayer_times();
    let Some(times) = times.filter(|_| st.prayer_enabled) else {
        // Off, or no place yet.
        let (msg, label) = if !st.prayer_enabled {
            ("Prayer times are off", "Turn on")
        } else {
            ("Set your city to see today's prayer times", "Open settings")
        };
        ui.text(msg, Rect::new(r.x, r.cy() - 26.0, r.w, 20.0), 14.0, SEMIBOLD, t.text, Align::Center);
        let bw = ui.measure(label, 12.0, SEMIBOLD) + 32.0;
        if ui.button(Rect::new(r.cx() - bw / 2.0, r.cy() + 6.0, bw, 30.0), label, ButtonStyle::Accent) {
            if !st.prayer_enabled {
                settings::update(|x| x.prayer_enabled = true);
                s.shown_at = Some(Instant::now());
            } else {
                *go = Some("settings");
            }
        }
        return;
    };

    let now = model::now_hours();
    let line = timeline(s, model::times_list(&times), now);
    if line.len() < SLOTS {
        return;
    }
    let intro = s.shown_at.map_or(10.0, |a| a.elapsed().as_secs_f32());
    ui.animate = true; // the sun breathes while the tab is open

    let body = r.pad(18.0, 0.0, 18.0, 4.0);
    let (left, right) = body.take_left(230.0, 0.0);
    summary(ui, left, &line, now, intro);

    // The timeline moved on (a prayer time passed): slide one step.
    let first = (line[0].0, (line[0].1 * 60.0).round() as i64);
    if s.first.is_some_and(|f| f != first) {
        s.slide_at = Some(Instant::now());
    }
    s.first = Some(first);
    arc(ui, s, right, &line, now, intro);
}

// ---------------------------------------------------------------- left: next prayer

fn summary(ui: &mut Ui, r: Rect, line: &[Slot], now: f64, intro: f32) {
    let t = ui.t;
    let next = line[1];
    let city = model::prayer_city();
    let place = if city.trim().is_empty() { "PRAYER".to_string() } else { format!("PRAYER · {}", city.trim().to_uppercase()) };

    let h = 132.0;
    let y0 = r.cy() - h / 2.0;
    let base = ui.alpha;
    // Rows rise in one after another.
    let rise = |ui: &mut Ui, i: usize| -> f32 {
        let p = ease_out((intro - 0.05 * i as f32) / 0.36);
        ui.alpha = base * p;
        10.0 * (1.0 - p)
    };
    let dy = rise(ui, 0);
    ui.text(&place, Rect::new(r.x, y0 + dy, r.w, 15.0), 11.0, BOLD, t.subtext, Align::Left);
    let dy = rise(ui, 1);
    ui.text("Next", Rect::new(r.x, y0 + 26.0 + dy, r.w, 16.0), 12.0, SEMIBOLD, t.subtext, Align::Left);
    let dy = rise(ui, 2);
    ui.text(NAMES[next.0], Rect::new(r.x - 1.0, y0 + 40.0 + dy, r.w, 42.0), 34.0, BOLD, t.text, Align::Left);
    let dy = rise(ui, 3);
    let at = prayer::hhmm(next.1);
    let tw = ui.measure(&at, 17.0, SEMIBOLD);
    ui.text(&at, Rect::new(r.x, y0 + 84.0 + dy, tw + 4.0, 24.0), 17.0, SEMIBOLD, t.text, Align::Left);
    ui.text(&format!("in {}", span(next.1 - now)), Rect::new(r.x + tw + 12.0, y0 + 85.0 + dy, r.w - tw - 12.0, 24.0), 14.0, SEMIBOLD, t.accent, Align::Left);
    let dy = rise(ui, 4);
    // Progress from the prayer we're in to the next one.
    let from = if line[0].1 <= now { line[0].1 } else { next.1 - 8.0 };
    let total = next.1 - from;
    let done = if total > 0.0 { ((now - from) / total).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let bar = Rect::new(r.x, y0 + 122.0 + dy, r.w - 24.0, 5.0);
    ui.fill(bar, 2.5, t.track);
    let grow = ease_out(intro / 0.9);
    let w = (5.0 + (bar.w * done - 5.0).max(0.0) * grow).min(bar.w);
    ui.fill(Rect::new(bar.x, bar.y, w, bar.h), 2.5, t.accent);
    ui.alpha = base;
}

// ---------------------------------------------------------------- right: the sun's path

fn arc(ui: &mut Ui, s: &mut State, r: Rect, line: &[Slot], now: f64, intro: f32) {
    let t = ui.t;
    let (w, h) = (r.w, r.h);
    if w < 80.0 || h < 60.0 {
        return;
    }
    let pad = 34.0;
    let label_h = 34.0;
    let top = 12.0;
    let bottom = h - label_h - 8.0;
    let baseline = r.y + ((top + bottom) / 2.0).round() + 0.5;
    let amp = (bottom - top) / 2.0;
    let step = (w - 2.0 * pad) / (SLOTS - 1) as f32;

    // Sliding one step left after a prayer time passed.
    let mut shift = 0.0;
    if let Some(at) = s.slide_at {
        let p = ease_out(at.elapsed().as_secs_f32() / 0.8);
        if p >= 1.0 {
            s.slide_at = None;
        } else {
            shift = step * (1.0 - p);
        }
    }
    let x_of = |i: usize| r.x + pad + i as f32 * step + shift;

    // Phases, unwrapped so they keep rising through the night (the sine repeats every 2).
    let mut phases = [0.0f64; SLOTS];
    for i in 0..SLOTS {
        phases[i] = sun_phase(line[i].0);
        while i > 0 && phases[i] <= phases[i - 1] {
            phases[i] += 2.0;
        }
    }
    let y_at = |x: f32| -> f32 {
        let f = ((x - r.x - pad - shift) / step) as f64;
        let i = (f.floor() as i64).clamp(0, SLOTS as i64 - 2) as usize;
        let ph = phases[i] + (phases[i + 1] - phases[i]) * (f - i as f64);
        baseline - amp * (std::f64::consts::PI * ph).sin() as f32
    };

    // Horizon, with a sun marking the day side and a moon the night side.
    let fade_in = ease_out(intro / 0.5);
    let base = ui.alpha;
    ui.alpha = base * fade_in;
    ui.g.line(r.x, baseline, r.right(), baseline, ui.c(with_alpha(t.subtext, 0.45)), 1.0);
    ui.icon("Sun", r.x + 7.5, baseline - 10.5, 11.0, with_alpha(t.subtext, 0.6));
    ui.icon("Moon", r.x + 7.5, baseline + 10.5, 11.0, with_alpha(t.subtext, 0.6));
    ui.alpha = base;

    // Where the sun is now: between the prayer we're in and the next, by time.
    let span_h = line[1].1 - line[0].1;
    let frac = if span_h > 0.0 { ((now - line[0].1) / span_h).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let sun_x = x_of(0) + step * frac;

    // The path, drawn in from the left on opening; the travelled part brighter.
    let x0 = x_of(0);
    let x_end = x_of(SLOTS - 1);
    let reveal = x0 + (x_end - x0 + 4.0) * ease_out(intro / 0.9);
    let lit_reveal = x0 + (x_end - x0 + 4.0) * ease_out(intro / 1.1);
    let build = |to: f32| {
        ui.g.path(|sink| unsafe {
            sink.BeginFigure(pt(x0, y_at(x0)), D2D1_FIGURE_BEGIN_HOLLOW);
            let mut x = x0 + 2.0;
            while x < to {
                sink.AddLine(pt(x, y_at(x)));
                x += 2.0;
            }
            sink.AddLine(pt(to, y_at(to)));
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        })
    };
    if let Some(g) = build(reveal.min(x_end)) {
        ui.g.stroke_geometry(&g.into(), ui.c(with_alpha(t.subtext, 0.35)), 1.6);
    }
    let lit_to = sun_x.min(lit_reveal);
    if lit_to > x0 + 0.5 {
        if let Some(g) = build(lit_to) {
            ui.g.stroke_geometry(&g.into(), ui.c(t.accent), 2.2);
        }
    }

    // Each prayer: a dot on the path and its name and time under it.
    let today_end = 24.0;
    for (i, &(pi, at)) in line.iter().enumerate() {
        let x = x_of(i);
        let y = y_at(x);
        let is_next = i == 1;
        let past = i == 0;
        let delay = 0.25 + 0.09 * i as f32;
        let scale = pop((intro - delay) / 0.38);
        if scale > 0.0 {
            let rad = if is_next { 5.0 } else { 3.5 } * scale;
            let fill = if is_next { t.text } else if past { t.subtext } else { t.surface };
            ui.g.fill_circle(x, y, rad, ui.c(fill));
            if !past && !is_next {
                ui.g.stroke_circle(x, y, rad - 0.6, ui.c(t.subtext), 1.2);
            }
        }
        if is_next && intro > delay {
            // A ring rippling out of the next prayer's dot.
            let p = ((intro - delay) / 2.2).fract();
            let k = 0.4 + p;
            ui.g.stroke_circle(x, y, 10.0 * k, ui.c(with_alpha(t.text, 0.7 * (1.0 - p))), 1.5);
        }
        let lp = ease_out((intro - delay) / 0.4);
        ui.alpha = base * lp;
        let ly = r.bottom() - label_h + 8.0 * (1.0 - lp);
        let lr = Rect::new(x - 40.0, ly, 80.0, 16.0);
        ui.text(NAMES[pi], lr, 11.5, if is_next { BOLD } else { REGULAR }, if is_next { t.text } else { t.subtext }, Align::Center);
        let time = if at >= today_end { format!("{} ›", prayer::hhmm(at)) } else { prayer::hhmm(at) };
        ui.text(&time, Rect::new(lr.x, ly + 16.0, lr.w, 15.0), 11.0, REGULAR, if is_next { t.accent } else { t.subtext }, Align::Center);
        ui.alpha = base;
    }

    // The sun now: a dot with a soft halo that breathes.
    let sp = pop((intro - 0.9) / 0.38);
    if sp > 0.0 {
        let sy = y_at(sun_x);
        let breathe = 1.0 + 0.25 * ((intro - 0.9) / 1.6 * std::f32::consts::PI).sin();
        let halo = 15.0 * breathe * sp.min(1.0);
        let rings = 12;
        for k in 0..rings {
            // Stacked translucent discs approximate a radial gradient (strong centre, clear edge).
            let rad = halo * (rings - k) as f32 / rings as f32;
            ui.g.fill_circle(sun_x, sy, rad, ui.c(with_alpha(t.accent, 0.56 / rings as f32 * 1.4)));
        }
        ui.g.fill_circle(sun_x, sy, 5.5 * sp, ui.c(t.accent));
    }
}
